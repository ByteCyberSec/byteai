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
Use tools (shell, read, search, edit, websearch, fetch, todo, note, memory) whenever they reduce guesswork.
Read files before editing them. Search before assuming. Use websearch+fetch for real-time web info. Run commands to verify.

Reporting rules:
- Final answer structure: 1) What changed  2) Verification  3) Blockers/risks  4) Next action.
- No preamble, no "Great question!", no repeating the user's request.
- Never claim success without verification. If you cannot verify, say what remains unverified.
- Keep responses concise. Use short sections, not giant paragraphs.

Chat mode: when the user's message is short, casual, or conversational (greetings, small talk, casual questions) — respond naturally without calling tools. Only use tools when the user gives a concrete task or asks for information that requires them.

Clarity: if the user's request is vague or underspecified, ask 1-2 brief clarifying questions before acting. Do NOT guess what the user wants.
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

    /// Inject relevant prior memories into the system prompt (mem0-style).
    /// Searches the memory store for entries matching the user's query and
    /// prepends a "Relevant prior knowledge" block. Silently skips when the
    /// memory store is unavailable or the query is empty.
    pub fn inject_memories(&mut self, query: &str) {
        if query.trim().is_empty() {
            return;
        }
        let mem = match apex_memory::Memory::open(&self.data_dir.join("memory")) {
            Ok(m) => m,
            Err(_) => return, // no memory store yet
        };
        // Collect meaningful keywords (drop stop words/short tokens), search
        // each individually so a match on any substantive term recalls the fact.
        let stop: &[&str] = &["the", "and", "for", "with", "what", "tell", "about", "please", "help", "this", "that", "your", "you", "are", "can", "how", "why", "when", "where", "which", "into"];
        let mut seen: Vec<apex_memory::Entry> = Vec::new();
        for word in query.split_whitespace() {
            let w = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if w.len() < 4 || stop.contains(&w.as_str()) {
                continue;
            }
            if let Ok(entries) = mem.search(&w, None, 3) {
                for e in entries {
                    if !seen.iter().any(|x| x.id == e.id) {
                        seen.push(e);
                    }
                }
            }
        }
        if seen.is_empty() {
            return;
        }
        let mut block = String::from("\n\nRelevant prior knowledge:\n");
        for e in seen.iter().take(5) {
            block.push_str(&format!("- {}: {}\n", e.title, e.body));
        }
        // Insert the memory block right after the system prompt.
        if let Some(sys) = self.history.first_mut() {
            if let Some(ref mut c) = sys.content {
                c.push_str(&block);
            }
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
        // mem0-style: recall relevant prior facts into the system prompt
        // so the agent doesn't start each session from scratch.
        self.inject_memories(user_input);
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
                self.extract_memories(&outcome).await;
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
        // mem0-style: auto-extract durable facts from this conversation turn
        // and persist them so the agent remembers across sessions.
        self.extract_memories(&outcome).await;
        Ok(outcome)
    }

    /// mem0-style auto-memory: extract durable facts from the just-completed
    /// turn and persist them to the memory store. Best-effort (silent on
    /// failure). The model is asked to extract 1-3 short factual statements
    /// (preferences, decisions, entity info) that should be remembered across
    /// sessions.
    async fn extract_memories(&mut self, outcome: &AgentOutcome) {
        // Only extract from successful turns with meaningful content.
        if !outcome.finished || outcome.final_text.is_empty() {
            return;
        }
        // Find the last user message (the one that triggered this turn).
        let last_user = self.history.iter().rev().find(|m| m.role == Role::User);
        let Some(user) = last_user else { return };
        let Some(user_text) = &user.content else { return };
        if user_text.trim().is_empty() {
            return;
        }
        // Use the provider to extract facts (small prompt, cheap model call).
        let prompt = format!(
            "Extract 1-3 concise, factual statements from this conversation exchange. \
             Focus on durable facts: user preferences, decisions made, project structure, \
             file paths, configuration values, or requirements. Each statement must be \
             self-contained and verifiable. Omit transient information.\n\n\
             User: {user_text}\n\n\
             Assistant: {}",
            outcome.final_text
        );
        let msg = Message::user(&prompt);
        let (text, _) = match self.provider.chat(&self.config.model, &[msg], &[], Some(256)).await {
            Ok((t, _, _)) if !t.trim().is_empty() => (t.trim().to_string(), true),
            _ => return,
        };
        // Save each line as a memory entity.
        let mut mem = match apex_memory::Memory::open(&self.data_dir.join("memory")) {
            Ok(m) => m,
            Err(_) => return,
        };
        for line in text.lines() {
            let line = line.trim().trim_start_matches(|c| c == '-' || c == '*' || c == ' ').trim();
            if line.is_empty() {
                continue;
            }
            let title = line.chars().take(60).collect::<String>();
            let _ = mem.upsert(apex_memory::Kind::Entity, &title, line, &[String::from("auto")], None);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use apex_memory::Kind;
    use apex_provider::Client;
    use apex_tools::Registry;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_core_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_agent(data_dir: &std::path::Path) -> Agent {
        let client = Client::new("http://127.0.0.1:1/v1", "test").unwrap();
        let tools = Registry::builtins(&apex_tools::ToolContext::new(data_dir.to_path_buf()));
        let cfg = AgentConfig::default();
        Agent::new(client, cfg, tools, data_dir.to_path_buf())
    }

    #[test]
    fn inject_memories_prepends_relevant_block() {
        let dir = tmp_dir("inject");
        // Seed one memory entity about "rust async".
        let mut mem = apex_memory::Memory::open(&dir.join("memory")).unwrap();
        mem.upsert(Kind::Entity, "rust async", "user prefers async rust", &[].to_vec(), None).unwrap();
        drop(mem);

        let mut agent = make_agent(&dir);
        agent.with_system_prompt();
        agent.inject_memories("tell me about async rust");

        let sys = agent.history[0].content.clone().unwrap_or_default();
        assert!(sys.contains("Relevant prior knowledge"));
        assert!(sys.contains("user prefers async rust"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inject_memories_skips_empty_query() {
        let dir = tmp_dir("inject_empty");
        let mut agent = make_agent(&dir);
        agent.with_system_prompt();
        agent.inject_memories("  ");
        let sys = agent.history[0].content.clone().unwrap_or_default();
        assert!(!sys.contains("Relevant prior knowledge"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inject_memories_silent_when_store_missing() {
        let dir = tmp_dir("inject_missing");
        let mut agent = make_agent(&dir); // no memory dir created
        agent.with_system_prompt();
        agent.inject_memories("some query here");
        let sys = agent.history[0].content.clone().unwrap_or_default();
        assert!(!sys.contains("Relevant prior knowledge"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
