//! The ByteAi (APEX) agent core: loop, state machine, context budget, tool
//! dispatch. Phase 1 keeps the loop lean; power modules attach later.

use std::time::Duration;

use anyhow::Result;
use apex_provider::{Client, StreamEvent};
use apex_tools::Registry;
use apex_types::{
    AgentOutcome, Message, Role, ToolCall, ToolOutcome, Usage,
};
use serde_json::Value;
use tracing::debug;

/// Agent execution phases (spec §45). Phase 1 uses a subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Understanding,
    Investigating,
    Implementing,
    Verifying,
    Recovering,
    Reviewing,
    Complete,
    Blocked,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Understanding => "UNDERSTANDING",
            Phase::Investigating => "INVESTIGATING",
            Phase::Implementing => "IMPLEMENTING",
            Phase::Verifying => "VERIFYING",
            Phase::Recovering => "RECOVERING",
            Phase::Reviewing => "REVIEWING",
            Phase::Complete => "COMPLETE",
            Phase::Blocked => "BLOCKED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Max model iterations per user turn (each iteration may dispatch tools).
    /// `0` = unlimited (use `run_budget_seconds` as the real backstop).
    pub max_iterations: u32,
    /// Token budget; compaction triggers above this estimate.
    pub context_budget_tokens: u64,
    /// Per-tool execution timeout.
    pub tool_timeout: Duration,
    /// Enable tool calling (models without tool support run chat-only).
    pub tools_enabled: bool,
    pub max_tokens: Option<u32>,
    /// Optional wall-clock budget for one user turn (seconds). Whichever of
    /// the iteration cap or this budget hits first triggers a graceful
    /// wrap-up (final answer from partial progress) — both are enforced,
    /// not either/or.
    pub run_budget_seconds: Option<u64>,
    /// Fraction of the iteration budget at which a proactive "begin wrapping
    /// up" notice is injected into the model context (0.8 = 80%). Disabled
    /// when max_iterations is 0.
    pub warn_ratio: f32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_iterations: 150,
            context_budget_tokens: 200_000,
            tool_timeout: Duration::from_secs(300),
            tools_enabled: true,
            max_tokens: None,
            run_budget_seconds: None,
            warn_ratio: 0.8,
        }
    }
}

/// System prompt: short, model-aware, ADHD-friendly output contract.
pub const SYSTEM_PROMPT: &str = r#"You are ByteAi (codename APEX), an autonomous coding agent.

Working style: UNDERSTAND → INVESTIGATE → IMPLEMENT → TEST → VERIFY → REPORT.
Use tools (shell, read, search, edit, todo, note) whenever they reduce guesswork.
Read files before editing them. Search before assuming. Run commands to verify.

Reporting rules:
- Final answer structure: 1) What changed  2) Verification  3) Blockers/risks  4) Next action.
- No preamble, no "Great question!", no repeating the user's request.
- Never claim success without verification. If you cannot verify, say what remains unverified.
- Keep responses concise. Use short sections, not giant paragraphs.
"#;

pub struct Agent {
    pub provider: Client,
    pub config: AgentConfig,
    pub tools: Registry,
    pub history: Vec<Message>,
    pub usage: Usage,
    pub phase: Phase,
    pub data_dir: std::path::PathBuf,
}

impl Agent {
    pub fn new(provider: Client, config: AgentConfig, tools: Registry, data_dir: std::path::PathBuf) -> Self {
        Self { provider, config, tools, history: Vec::new(), usage: Usage::default(), phase: Phase::Understanding, data_dir }
    }

    pub fn with_system_prompt(&mut self) {
        if !self.history.iter().any(|m| m.role == Role::System) {
            self.history.insert(0, Message::system(SYSTEM_PROMPT));
        }
    }

    /// One user turn: stream the model, dispatch tool calls, loop until done.
    /// Generic callbacks (instead of `&mut dyn FnMut`) so the future is `Send`
    /// and can run in a background task (used by the TUI).
    pub async fn run<F1, F2>(
        &mut self,
        user_input: &str,
        on_text: &mut F1,
        on_tool: &mut F2,
    ) -> Result<AgentOutcome>
    where
        F1: FnMut(&str) + Send,
        F2: FnMut(&ToolOutcome) + Send,
    {
        self.with_system_prompt();
        self.history.push(Message::user(user_input));
        self.phase = Phase::Understanding;

        let mut outcome = AgentOutcome::default();
        let mut tools_active = self.config.tools_enabled;
        let started = std::time::Instant::now();
        let capped = self.config.max_iterations;
        let wall = self.config.run_budget_seconds;
        let mut warned = false;
        let mut exhaustion: Option<String> = None;

        loop {
            // --- Interaction budgets (whichever hits first; 0 = unlimited) ---
            if capped > 0 && outcome.iterations >= capped {
                exhaustion = Some(format!("iteration budget ({capped} iters)"));
                break;
            }
            if let Some(b) = wall {
                if started.elapsed().as_secs() >= b {
                    exhaustion = Some(format!("wall-clock run budget ({b}s)"));
                    break;
                }
            }
            // --- Proactive wrap-up nudge before the cap is hit (beyond Hermes) ---
            if capped > 0 && !warned {
                let threshold = (capped as f32 * self.config.warn_ratio).max(1.0) as u32;
                if outcome.iterations + 1 >= threshold {
                    warned = true;
                    self.history.push(Message::system(&format!(
                        "Interaction budget notice: you are near the {capped}-iteration cap \
                         (about to use {}/{}). If you already have enough to answer, STOP \
                         calling tools and give your final answer now.",
                        outcome.iterations + 1, capped
                    )));
                }
            }

            outcome.iterations += 1;
            self.enforce_budget();
            let defs = if tools_active { self.tools.defs() } else { Vec::new() };

            let mut content = String::new();
            let mut reasoning = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut usage_this_turn: Option<Usage> = None;

            let stream_result = self
                .provider
                .chat_stream(&self.config.model, &self.history, &defs, self.config.max_tokens, |ev| {
                    match ev {
                        StreamEvent::Content(c) => {
                            content.push_str(&c);
                            on_text(&c);
                        }
                        StreamEvent::Reasoning(r) => reasoning.push_str(&r),
                        StreamEvent::ToolCallDelta(index, id, name, args) => {
                            while calls.len() <= index {
                                calls.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() });
                            }
                            let entry = &mut calls[index];
                            if !id.is_empty() { entry.id = id; }
                            if !name.is_empty() { entry.name = name; }
                            entry.arguments.push_str(&args);
                        }
                        StreamEvent::Usage(u) => usage_this_turn = Some(u),
                        StreamEvent::Done => {}
                    }
                })
                .await;

            match stream_result {
                Ok(()) => {}
                Err(e) => {
                    // Failure recovery (spec §23): classify and report; no blind retry.
                    let reason = classify(&e);
                    self.phase = Phase::Blocked;
                    outcome.finished = false;
                    outcome.blocked_reason = Some(format!("{reason}: {e:#}"));
                    // Keep the failed exchange out of history to allow a clean retry.
                    self.history.pop();
                    return Ok(outcome);
                }
            }

            if let Some(u) = usage_this_turn {
                if u.total_tokens > 0 {
                    self.usage.add(&u);
                } else {
                    // Provider returned zero usage — estimate instead.
                    let est = (content.len() / 4) as u64 + estimate_tokens(&self.history);
                    self.usage.total_tokens += est;
                }
                outcome.usage = self.usage.clone();
            } else {
                let est = (content.len() / 4) as u64 + estimate_tokens(&self.history);
                self.usage.total_tokens += est;
                outcome.usage = self.usage.clone();
            }

            let valid_calls: Vec<ToolCall> = calls
                .into_iter()
                .filter(|c| !c.name.is_empty())
                .map(|mut c| {
                    if c.id.is_empty() {
                        c.id = format!("call_{}", c.name);
                    }
                    c
                })
                .collect();

            if valid_calls.is_empty() {
                // Normal completion.
                self.history.push(Message::assistant(
                    if content.is_empty() { None } else { Some(content.clone()) },
                    None,
                    if reasoning.is_empty() { None } else { Some(reasoning) },
                ));
                self.phase = Phase::Complete;
                outcome.final_text = content;
                outcome.finished = true;
                return Ok(outcome);
            }

            // Tool round: dispatch all calls in parallel.
            self.phase = Phase::Implementing;
            outcome.tool_calls_made += valid_calls.len() as u32;
            self.history.push(Message::assistant(
                if content.is_empty() { None } else { Some(content) },
                Some(valid_calls.clone()),
                if reasoning.is_empty() { None } else { Some(reasoning) },
            ));

            let mut outcomes = Vec::with_capacity(valid_calls.len());
            for call in &valid_calls {
                let tool = self.tools.get(&call.name);
                let mut outcome = match tool {
                    Some(t) => {
                        let args: Value = serde_json::from_str(&call.arguments)
                            .unwrap_or_else(|_| serde_json::json!({"error": format!("unparseable args: {}", call.arguments)}));
                        let res = tokio::time::timeout(self.config.tool_timeout, t.execute(args)).await;
                        match res {
                            Ok(r) => r,
                            Err(_) => ToolOutcome {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                output: format!("ERROR: tool timeout after {}s", self.config.tool_timeout.as_secs()),
                                ok: false,
                                elapsed_ms: self.config.tool_timeout.as_millis() as u64,
                            },
                        }
                    }
                    None => ToolOutcome {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        output: format!("ERROR: unknown tool {:?}. Available: {}", call.name, self.tools.names().join(", ")),
                        ok: false,
                        elapsed_ms: 0,
                    },
                };
                // Stamp the real call id — tools don't know it, and providers
                // reject tool messages with an empty/missing tool_call_id.
                outcome.call_id = call.id.clone();
                debug!("tool {} -> ok={} {}ms", call.name, outcome.ok, outcome.elapsed_ms);
                on_tool(&outcome);
                outcomes.push(outcome);
            }

            // If every tool failed with "unknown tool", disable tools to avoid a retry loop.
            if !outcomes.is_empty() && outcomes.iter().all(|o| !o.ok) && outcomes.iter().any(|o| o.output.contains("unknown tool")) {
                tools_active = false;
                self.phase = Phase::Recovering;
            }

            for o in &outcomes {
                self.history.push(Message::tool(&o.call_id, &o.name, &o.output));
            }
        }

        self.phase = Phase::Recovering;

        // --- Graceful budget exhaustion (beyond Hermes: we still deliver a
        // real final answer from partial progress, and record WHY we stopped) ---
        let reason = exhaustion.unwrap_or_else(|| format!("iteration budget ({capped} iters)"));
        self.history.push(Message::system(&format!(
            "Reached the {reason}. Do NOT call any tools. Provide your best final \
             answer now, summarizing what you found and completed so far."
        )));
        let defs: Vec<apex_types::ToolDef> = Vec::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        match self
            .provider
            .chat_stream(&self.config.model, &self.history, &defs, self.config.max_tokens, |ev| {
                match ev {
                    StreamEvent::Content(c) => {
                        content.push_str(&c);
                        on_text(&c);
                    }
                    StreamEvent::Reasoning(r) => reasoning.push_str(&r),
                    _ => {}
                }
            })
            .await
        {
            Ok(()) => {}
            Err(e) => {
                // Drop the wrap-up system message so the history stays clean.
                self.history.pop();
                self.phase = Phase::Blocked;
                outcome.finished = false;
                outcome.blocked_reason = Some(format!("{reason}: wrap-up failed: {e:#}"));
                outcome.exhausted = true;
                outcome.exhausted_reason = Some(reason);
                return Ok(outcome);
            }
        }
        outcome.iterations += 1;
        let est = (content.len() / 4) as u64 + estimate_tokens(&self.history);
        self.usage.total_tokens += est;
        outcome.usage = self.usage.clone();
        if !content.trim().is_empty() {
            self.history.push(Message::assistant(
                Some(content.clone()),
                None,
                if reasoning.is_empty() { None } else { Some(reasoning) },
            ));
            self.phase = Phase::Complete;
            outcome.final_text = content;
            outcome.finished = true;
        } else {
            self.phase = Phase::Blocked;
            outcome.finished = false;
            outcome.blocked_reason = Some(format!("{reason}: no final answer produced"));
        }
        outcome.exhausted = true;
        outcome.exhausted_reason = Some(reason);
        Ok(outcome)
    }

    /// Minimal budget enforcement: when the estimated history exceeds the
    /// budget, drop old tool-result messages (keep system + last N turns).
    /// Full compaction (jcode ladder + aux summarization) lands in Phase 2.
    fn enforce_budget(&mut self) {
        let mut est = estimate_tokens(&self.history);
        if est <= self.config.context_budget_tokens {
            return;
        }
        let keep = 12; // system + last ~6 turns
        let i = 1; // keep index 0 (system); removals shift items down, i stays at 1
        while i < self.history.len().saturating_sub(keep) && est > self.config.context_budget_tokens * 8 / 10 {
            let _dropped = self.history.remove(i);
            est = estimate_tokens(&self.history);
        }
    }
}

/// Estimate tokens cheaply (chars / 4) — used when the provider omits usage.
pub fn estimate_tokens(messages: &[Message]) -> u64 {
    let mut chars = 0usize;
    for m in messages {
        if let Some(c) = &m.content {
            chars += c.len();
        }
        if let Some(r) = &m.reasoning {
            chars += r.len();
        }
        if let Some(tc) = &m.tool_calls {
            for t in tc {
                chars += t.name.len() + t.arguments.len();
            }
        }
    }
    (chars / 4) as u64
}

/// Failure classification (spec §23). Phase 1: coarse classes; the full
/// taxonomy (syntax/tool/permission/dependency/assumption/test/env/network/
/// edit-conflict/formatting) lands with the retry engine.
pub fn classify(e: &anyhow::Error) -> &'static str {
    let s = format!("{e:#}");
    let low = s.to_lowercase();
    if low.contains("401") || low.contains("auth") {
        "AUTH"
    } else if low.contains("429") || low.contains("rate") || low.contains("quota") {
        "RATE_LIMIT"
    } else if low.contains("timeout") || low.contains("timed out") {
        "TIMEOUT"
    } else if low.contains("connection") || low.contains("dns") || low.contains("refused") {
        "NETWORK"
    } else if low.contains("400") || low.contains("invalid") || low.contains("context") {
        "REQUEST"
    } else {
        "UNKNOWN"
    }
}
