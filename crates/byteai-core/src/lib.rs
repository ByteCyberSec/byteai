//! The ByteAi agent core: loop, state machine, context budget, tool
//! dispatch. Phase 1 keeps the loop lean; power modules attach later.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use byteai_provider::{pool::ProviderPool, StreamEvent};

pub mod toolselect;
pub use toolselect::{ToolSelectStrategy, select_tools};
pub mod sanitize;
use byteai_tools::Registry;
use byteai_types::{
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
    /// The model asked the user a question and the turn is PAUSED waiting
    /// for the human's answer (CAP — Coding Auto-Pilot — off).
    AwaitingInput,
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
            Phase::AwaitingInput => "AWAITING_INPUT",
            Phase::Blocked => "BLOCKED",
        }
    }
}

/// Per-phase spinner sets: each phase gets its own icon + ASCII/braille frame
/// set so the user can tell at a glance what the agent is doing (thinking,
/// searching, writing, testing, recovering, reviewing).
pub struct SpinStyle {
    pub icon: &'static str,
    pub frames: &'static [char],
}

pub fn spinner_for(phase: Phase) -> &'static SpinStyle {
    match phase {
        Phase::Understanding => &SpinStyle { icon: "⋯", frames: &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'] },
        Phase::Investigating => &SpinStyle { icon: "◎", frames: &['◐', '◓', '◑', '◒'] },
        Phase::Implementing => &SpinStyle { icon: "✎", frames: &['▖', '▘', '▝', '▗'] },
        Phase::Verifying => &SpinStyle { icon: "✓", frames: &['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'] },
        Phase::Recovering => &SpinStyle { icon: "⚠", frames: &['♲', '✇', '♻'] },
        Phase::Reviewing => &SpinStyle { icon: "✉", frames: &['▤', '▥', '▦', '▧'] },
        Phase::Complete => &SpinStyle { icon: "✓", frames: &['●'] },
        Phase::AwaitingInput => &SpinStyle { icon: "✋", frames: &['?', '？', '?'] },
        Phase::Blocked => &SpinStyle { icon: "✗", frames: &['●'] },
    }
}

/// Live status shared between the agent core and any UI (TUI, REPL, remote).
/// Updated by `Agent::run` on every phase change, iteration, and tool call so
/// the UI can render a Hermes-style live activity line. `Arc<Mutex<..>>` so
/// the UI reads it lock-free (try_lock) on every frame.
#[derive(Debug, Clone)]
pub struct LiveStatus {
    pub phase: Phase,
    /// Tools currently executing (set right before dispatch, cleared after).
    pub active_tools: Vec<String>,
    /// Iterations used this turn.
    pub iterations: u32,
    /// Per-turn iteration cap (0 = unlimited), for "iter N/cap" display.
    pub iter_cap: u32,
    /// Human-readable note for the current activity (e.g. exhaustion reason).
    pub note: String,
}

impl Default for LiveStatus {
    fn default() -> Self {
        Self {
            phase: Phase::Understanding,
            active_tools: Vec::new(),
            iterations: 0,
            iter_cap: 0,
            note: String::new(),
        }
    }
}

impl LiveStatus {
    /// Render a single-line activity status (no elapsed — UI adds that).
    pub fn line(&self, spinner_frame: usize) -> String {
        let style = spinner_for(self.phase);
        let sp = style.frames[(spinner_frame / 2) % style.frames.len().max(1)];
        let tools = if self.active_tools.is_empty() {
            "…".to_string()
        } else {
            self.active_tools.join(", ")
        };
        let cap = if self.iter_cap > 0 {
            format!("/{}", self.iter_cap)
        } else {
            String::new()
        };
        let note = if self.note.is_empty() {
            String::new()
        } else {
            format!("  ({})", self.note)
        };
        format!(
            "{} {} {} {} · iter {}{cap}{note}",
            style.icon,
            sp,
            self.phase.as_str(),
            tools,
            self.iterations
        )
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
    /// Hard cap on how many chars of a tool result are injected into the
    /// model context. Tools like `skills` (this machine has 7000+ skills)
    /// can return megabytes; a giant blob blows the context budget and the
    /// provider window, stalling the turn. We truncate with a clear note and
    /// keep a compact head/tail so the model still sees what happened.
    pub max_tool_output_chars: usize,
    /// Auto-continue: when the model stops with no tool calls but the task
    /// isn't actually finished (it often emits a partial summary mid-task),
    /// run one cheap completion probe and, if the model says CONTINUE, feed
    /// a nudge back into history and keep looping (bounded by max_iterations
    /// and a stuck-loop guard). Disable to restore strict stop-on-first-text.
    pub auto_continue: bool,
    /// CAP — Coding Auto-Pilot. When OFF (default), a model response that is
    /// a question to the user (clarification / a choice) PAUSES the turn:
    /// the agent waits for the user's answer instead of answering its own
    /// question and plowing ahead. When ON, byteai never pauses for input —
    /// it decides autonomously and keeps working until the task is done.
    pub cap_enabled: bool,
    /// Smart Tool Selection: expose only the tools relevant to the current
    /// task instead of every tool def every call (kills context rot and
    /// prompt bloat — adapted from isair/jarvis, OpenJarvis-style ROUTE).
    pub tool_select: bool,
    /// Cap on tools exposed per turn when `tool_select` is on.
    pub tool_select_max: usize,
    /// TencentDB Agent Memory integration (L0-L3 team memory hub). When
    /// `enabled`, the agent recalls L2/L3 memory + relevant skills into the
    /// system prompt before each turn and captures L0 dialogue afterwards.
    pub tdai: byteai_memory::tdai::TdaiConfig,
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
            max_tool_output_chars: 50_000,
            auto_continue: true,
            cap_enabled: false,
            tool_select: true,
            tool_select_max: toolselect::DEFAULT_MAX,
            tdai: byteai_memory::tdai::TdaiConfig::default(),
        }
    }
}

/// System prompt: short, model-aware, ADHD-friendly output contract.
pub const SYSTEM_PROMPT: &str = r#"You are ByteAi, an autonomous coding agent.

Working style: UNDERSTAND → INVESTIGATE → IMPLEMENT → TEST → VERIFY → REPORT.
Use tools (shell, read, search, edit, websearch, fetch, todo, note, memory) whenever they reduce guesswork.
Read files before editing them. Search before assuming. Use websearch+fetch for real-time web info. Run commands to verify.

Task completion: when the user gives a task, DO NOT stop or summarize partway. Keep calling tools
until EVERY part of the request is implemented, tested, and verified. A response with no tool
calls is only acceptable when the entire task is truly complete — or you are genuinely blocked
(missing information, impossible request), in which case say exactly what you need and stop.
Do not pause after each step expecting approval; work autonomously to the finish.

Reporting rules:
- Final answer structure: 1) What changed  2) Verification  3) Blockers/risks  4) Next action.
- No preamble, no "Great question!", no repeating the user's request.
- Never claim success without verification. If you cannot verify, say what remains unverified.
- Keep responses concise. Use short sections, not giant paragraphs.

Chat mode: when the user's message is short, casual, or conversational (greetings, small talk, casual questions) — respond naturally without calling tools. Only use tools when the user gives a concrete task or asks for information that requires them.

Clarity: if the user's request is vague or underspecified, ask 1-2 brief clarifying questions before acting. Do NOT guess what the user wants.

OPERATING METHODOLOGY (Dan brief — @indydevdan's agentic engineering, applied to every task):
1. PLAN FIRST: before any non-trivial change, produce a plan (files to touch, order, risks, test strategy) — then execute mechanically. Use the plan/todo tools.
2. HARNESS OVER MODEL: use your sharpest tools, not raw shell. Skills + tools + context win tasks; consult the dan_methodology tool whenever unsure how to structure agent work.
3. SCALE COMPUTE, SHOW UP AT PLAN & REVIEW: delegate parallelizable work to subagents (spawn tool) in isolated worktrees/sandboxes; review everything before merge — "if you can't review it, the agent didn't do it."
4. SAFETY: sandbox execution, gate destructive commands, never touch production without review. Kill the raw-shell habit; use narrow tools.
5. CONTEXT HYGIENE: keep the window lean (memsearch + memory instead of re-reading everything); compact long sessions.
6. ROUTE BY TASK: small/fast model for routine work, flagship for planning/review (route/moa/council tools).
7. LEARN: after hard tasks, capture the working solution into a skill (skills tool) so the next task is faster.
Call `dan_methodology` (tool) or `/dan` (TUI) for deep sections on any of these — harness_engineering, skills_system, agent_sandboxes, multi_agent, model_selection, security, observability, context_engineering, planning, local_models, agent_threads, software_factory, prompt_engineering, agents_learning.
"#;

pub struct Agent {
    pub pool: ProviderPool,
    pub config: AgentConfig,
    pub tools: Registry,
    pub history: Vec<Message>,
    pub usage: Usage,
    pub phase: Phase,
    /// Live activity status shared with any UI (TUI/REPL poll this on every
    /// frame to render "what is the agent doing right now").
    pub live: Arc<Mutex<LiveStatus>>,
    pub data_dir: std::path::PathBuf,
}

impl Agent {
    pub fn new(pool: ProviderPool, config: AgentConfig, tools: Registry, data_dir: std::path::PathBuf) -> Self {
        let live = Arc::new(Mutex::new(LiveStatus {
            phase: Phase::Understanding,
            iter_cap: config.max_iterations,
            ..LiveStatus::default()
        }));
        Self { pool, config, tools, history: Vec::new(), usage: Usage::default(), phase: Phase::Understanding, live, data_dir }
    }

    /// Set the execution phase in both `self.phase` and the live status.
    fn set_phase(&mut self, p: Phase) {
        self.phase = p;
        if let Ok(mut l) = self.live.try_lock() {
            l.phase = p;
        }
    }

    /// One cheap completion probe (auto-continue). The model just emitted a
    /// no-tool response; ask it whether the ORIGINAL task is truly finished.
    /// Returns true = DONE (complete the turn), false = CONTINUE (keep
    /// working). A probe failure returns false (CONTINUE) — the stuck-loop
    /// guard in `run()` (no_tool_streak < 3) still bounds how many times we
    /// retry, so a transient probe error (rate limit, network blip) can never
    /// falsely end a task that still has work remaining. We only stop on a
    /// probe failure if the model has already claimed CONTINUE 3 times in a
    /// row without making tool progress.
    async fn check_done(&self, last_output: &str) -> bool {
        let prompt = format!(
            "You are the completion gate for a multi-step task. The agent's latest \
             response contained NO tool calls. It was:\n\n{last_output}\n\n\
             Is the ORIGINAL user task fully complete — every part implemented and \
             verified — or is the agent stopping early with work still remaining? \
             Reply with EXACTLY one word: DONE or CONTINUE."
        );
        let sanitized = sanitize::sanitize(&[Message::user(&prompt)]);
        // Reasoning models (DeepSeek thinking mode, Qwen3, GLM…) burn output
        // budget on reasoning_content BEFORE the visible answer. 8 tokens was
        // enough for "DONE" on a plain model but a thinking model consumed all
        // 8 (then 32) tokens thinking, so the verdict was never emitted and the
        // probe always read CONTINUE — auto-continue then nudged forever and
        // the turn never finished cleanly. 128 tokens leaves room for the
        // thinking tail plus the one-word verdict.
        let probe = self.pool.client().chat(self.pool.model(), &sanitized, &[], Some(128));
        // Bound the meta-call so a slow provider can't hang the turn; on
        // timeout treat as CONTINUE (never let the task die on a blip).
        match tokio::time::timeout(self.config.tool_timeout, probe).await {
            Ok(Ok((text, _, _))) => is_done_verdict(&text),
            Ok(Err(_)) | Err(_) => false, // probe failed: CONTINUE, let the streak guard bound it
        }
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
        let mem = match byteai_memory::Memory::open(&self.data_dir.join("memory")) {
            Ok(m) => m,
            Err(_) => return, // no memory store yet
        };
        // Collect meaningful keywords (drop stop words/short tokens), search
        // each individually so a match on any substantive term recalls the fact.
        let stop: &[&str] = &["the", "and", "for", "with", "what", "tell", "about", "please", "help", "this", "that", "your", "you", "are", "can", "how", "why", "when", "where", "which", "into"];
        let mut seen: Vec<byteai_memory::Entry> = Vec::new();
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
        if let Some(sys) = self.history.first_mut()
            && let Some(ref mut c) = sys.content {
                c.push_str(&block);
            }
    }

    /// Inject relevant skills matching the user's task into the system prompt
    /// (Hermes parity — mandatory skill loading). Scans the skills directory
    /// for skills whose name/description/body match the query, and appends
    /// their instructions as a "Relevant skills" block so the model knows
    /// which procedures to follow without being told.
    pub fn inject_skills(&mut self, query: &str) {
        let skills_root = self.data_dir.join("skills");
        if !skills_root.is_dir() {
            return;
        }
        let relevant = byteai_tools::skills::find_relevant(&skills_root, query, 5);
        if relevant.is_empty() {
            return;
        }
        let mut block = String::from("\n\nRelevant skills for this task:\n");
        for skill in &relevant {
            // Truncate body to keep overhead manageable (200 chars max).
            let body_preview: String = skill.body
                .chars()
                .take(200)
                .collect::<String>()
                .lines()
                .take(6)
                .collect::<Vec<_>>()
                .join("\n");
            block.push_str(&format!(
                "--- {} ---\n{}\n",
                skill.name,
                body_preview
            ));
        }
        block.push_str("--- end skills ---\n");
        // Insert right after the system prompt.
        if let Some(sys) = self.history.first_mut()
            && let Some(ref mut c) = sys.content {
                c.push_str(&block);
            }
    }

    /// Remove previously-injected memory/skills blocks from the system prompt
    /// so re-injection across turns is idempotent. Without this, every
    /// `run()` call appends another "Relevant prior knowledge" + skills +
    /// memory block to history[0], and a long REPL session grows the system
    /// prompt unboundedly (stale memories + context rot + token waste).
    /// Called at the start of each turn, before the injectors re-add fresh
    /// blocks. Truncates at the FIRST injected-block marker — every block is
    /// appended at the tail, so cutting at the earliest marker removes all
    /// previously injected blocks (and only those).
    fn strip_injected_blocks(&mut self) {
        if let Some(byteai_types::Message { role: Role::System, content: Some(c), .. }) = self.history.first_mut() {
            const MARKERS: [&str; 3] = [
                "\n\nRelevant prior knowledge:",
                "\n\nRelevant skills for this task:",
                "\n[Memory L3 core persona]",
            ];
            let mut first: Option<usize> = None;
            for m in MARKERS {
                if let Some(p) = c.find(m) {
                    first = Some(first.map_or(p, |f: usize| f.min(p)));
                }
            }
            if let Some(p) = first {
                c.truncate(p);
            }
        }
    }

    /// TencentDB Agent Memory recall (NATIVE — local-first, no external
    /// service): fetch the agent's L3 core persona, L2 scenario files, L1
    /// atomics, and skills matching the user's task from byteai's own SQLite
    /// hub, then inject them into the system prompt. Best-effort — silently
    /// skips when disabled or the hub isn't reachable.
    pub async fn inject_tdai_memory(&mut self, query: &str) {
        if !self.config.tdai.enabled {
            return;
        }
        let hub = match byteai_memory::hub::MemoryHub::open(&self.data_dir.join("memory")) {
            Ok(h) => h,
            Err(_) => return,
        };
        let mut block = String::new();

        // L3 core persona — the agent's durable identity.
        if let Ok(Some(core)) = hub.core_read()
            && !core.content.trim().is_empty() {
            block.push_str("\n\n[Memory L3 core persona]\n");
            block.push_str(&core.content);
            block.push('\n');
        }

        // L2 scenario files — durable context the team has curated.
        if let Ok(entries) = hub.scenario_ls()
            && !entries.is_empty() {
            let mut scene = String::new();
            for e in entries.iter().take(8) {
                scene.push_str(&format!("\n--- {} ---\n{}\n", e.path, e.content));
            }
            block.push_str("\n[Memory L2 scenarios]\n");
            block.push_str(&scene);
        }

        // L1 atomics — relevant distilled memories.
        if !query.trim().is_empty()
            && let Ok(atoms) = hub.atomic_search(query, 5)
            && !atoms.is_empty() {
            let mut a = String::new();
            for it in atoms.iter().take(5) {
                a.push_str(&format!("- [{}] {}\n", it.mem_type, it.content));
            }
            block.push_str("\n[Memory L1 atomics]\n");
            block.push_str(&a);
        }

        // Relevant skills from the hub's skill memory.
        if !query.trim().is_empty()
            && let Ok(skills) = hub.skill_search(query, 3)
            && !skills.is_empty() {
            let mut sk = String::new();
            for s in skills.iter().take(3) {
                let preview: String = s.content.chars().take(400).collect();
                sk.push_str(&format!("--- {} ---\n{}\n", s.name, preview));
            }
            block.push_str("\n[Memory skills]\n");
            block.push_str(&sk);
        }

        if block.is_empty() {
            return;
        }
        block.push_str("--- end memory ---\n");
        if let Some(sys) = self.history.first_mut()
            && let Some(ref mut c) = sys.content {
                c.push_str(&block);
            }
    }

    /// TencentDB Agent Memory capture (NATIVE): persist the just-completed
    /// turn's dialogue (L0) into byteai's own SQLite hub so durable facts can
    /// be distilled. Best-effort — silent on failure.
    pub async fn capture_tdai_turn(&mut self, user_input: &str, assistant_text: &str) {
        if !self.config.tdai.enabled {
            return;
        }
        let mut hub = match byteai_memory::hub::MemoryHub::open(&self.data_dir.join("memory")) {
            Ok(h) => h,
            Err(_) => return,
        };
        let session_id = if self.config.tdai.conversation_id.is_empty() {
            "default-conversation"
        } else {
            &self.config.tdai.conversation_id
        };
        let mut msgs: Vec<(&str, &str)> = vec![("user", user_input)];
        if !assistant_text.trim().is_empty() {
            msgs.push(("assistant", assistant_text));
        }
        let _ = hub.conversation_add(session_id, &msgs);
    }

    /// Return a sanitized clone of the history (secrets redacted, null bytes
    /// stripped) for sending to the provider. The original history is
    /// preserved intact so the agent can still read real values.
    fn sanitized_history(&self) -> Vec<Message> {
        sanitize::sanitize(&self.history)
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
        // Remove previously-injected memory/skills blocks so re-injection is
        // idempotent (the system prompt must not grow across turns).
        self.strip_injected_blocks();
        // mem0-style: recall relevant prior facts into the system prompt
        // so the agent doesn't start each session from scratch.
        self.inject_memories(user_input);
        // Hermes-parity: auto-load relevant skills into the system prompt so
        // the model knows which procedures to follow for this task.
        self.inject_skills(user_input);
        // TencentDB Agent Memory: recall L3 persona + L2 scenarios + relevant
        // skills from the team memory hub.
        self.inject_tdai_memory(user_input).await;
        // If a previous turn was interrupted (aborted mid-stream), its user
        // message is still dangling at the tail of history with no assistant
        // reply. Dropping it here prevents consecutive user turns on the next
        // run, which providers reject or models misinterpret.
        if let Some(byteai_types::Message { role: Role::User, .. }) = self.history.last() {
            self.history.pop();
        }
        self.history.push(Message::user(user_input));
        self.set_phase(Phase::Understanding);

        let mut outcome = AgentOutcome::default();
        let mut tools_active = self.config.tools_enabled;

        // Smart Tool Selection ("unlimited tools without context rot", adapted
        // from isair/jarvis; OpenJarvis-style ROUTE step): score the tool
        // registry against the user's task and expose only the relevant
        // subset instead of every tool def every call. The set GROWS on
        // demand — if the model calls a tool outside the selection, its def
        // is added back for the next iteration.
        let mut defs: Vec<byteai_types::ToolDef> = if tools_active {
            if self.config.tool_select {
                toolselect::select_tools(
                    &self.tools.defs(),
                    user_input,
                    toolselect::ToolSelectStrategy::Auto,
                    self.config.tool_select_max,
                )
            } else {
                self.tools.defs()
            }
        } else {
            Vec::new()
        };

        let started = std::time::Instant::now();
        let capped = self.config.max_iterations;
        let wall = self.config.run_budget_seconds;
        let mut warned = false;
        // Auto-continue guard: how many consecutive no-tool responses the
        // model has produced while claiming "not done yet". If it claims
        // CONTINUE 3 times in a row without making progress, it's stuck —
        // force a clean wrap-up instead of burning the iteration budget.
        let mut no_tool_streak: u32 = 0;
        let exhaustion: Option<String>;

        loop {
            // --- Interaction budgets (whichever hits first; 0 = unlimited) ---
            if capped > 0 && outcome.iterations >= capped {
                exhaustion = Some(format!("iteration budget ({capped} iters)"));
                break;
            }
            if let Some(b) = wall
                && started.elapsed().as_secs() >= b {
                    exhaustion = Some(format!("wall-clock run budget ({b}s)"));
                    break;
                }
            // --- Proactive wrap-up nudge before the cap is hit (beyond Hermes) ---
            if capped > 0 && !warned {
                let threshold = (capped as f32 * self.config.warn_ratio).max(1.0) as u32;
                if outcome.iterations + 1 >= threshold {
                    warned = true;
                    self.history.push(Message::system(format!(
                        "Interaction budget notice: you are near the {capped}-iteration cap \
                         (about to use {}/{}). Keep working — do NOT stop early — but \
                         prefer to consolidate: if the remaining work is small, finish it \
                         and give your final answer; otherwise continue calling tools.",
                        outcome.iterations + 1, capped
                    )));
                }
            }

            outcome.iterations += 1;
            if let Ok(mut l) = self.live.try_lock() {
                l.iterations = outcome.iterations;
                l.iter_cap = capped;
                l.phase = self.phase;
            }
            self.enforce_budget().await;
            // Uses the precomputed Smart Tool Selection (`defs`), which is
            // grown on demand after each tool round and cleared if tools get
            // disabled mid-turn.

            let mut content = String::new();
            let mut reasoning = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut usage_this_turn: Option<Usage> = None;

            // Reflect what the agent is actually doing: it is now reasoning over
            // the accumulated tool results (Investigating) rather than sitting
            // frozen on "Understanding" for the whole stream wait.
            if outcome.iterations > 1 {
                self.set_phase(Phase::Investigating);
            }

            // Sanitize a clone of the history for the wire: redact secrets,
            // strip null bytes. The original history keeps real values.
            let wire_history = self.sanitized_history();
            let stream_result = self
                .pool
                .client()
                .chat_stream(self.pool.model(), &wire_history, &defs, self.config.max_tokens, |ev| {
                    match ev {
                        StreamEvent::Content(c) => {
                            // First content token: the model is answering —
                            // move off the "Understanding" placeholder so the
                            // live status shows the real activity.
                            if content.is_empty()
                                && let Ok(mut l) = self.live.try_lock() {
                                    l.phase = Phase::Verifying;
                                }
                            content.push_str(&c);
                            on_text(&c);
                        }
                        StreamEvent::Reasoning(r) => {
                            // First reasoning token: model is thinking over
                            // results — reflect that as Investigating.
                            if reasoning.is_empty()
                                && let Ok(mut l) = self.live.try_lock() {
                                    l.phase = Phase::Investigating;
                                }
                            reasoning.push_str(&r);
                        }
                        StreamEvent::ToolCallDelta(index, id, name, args) => {
                            // The model is emitting tool calls: flip to Implementing
                            // immediately so the UI shows "✎ IMPLEMENTING <tool>"
                            // even before dispatch begins.
                            if !name.is_empty()
                                && let Ok(mut l) = self.live.try_lock() {
                                    l.phase = Phase::Implementing;
                                    if !l.active_tools.contains(&name.to_string()) {
                                        l.active_tools.push(name.to_string());
                                    }
                                }
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
                Ok(()) => {
                    // The active provider completed a stream successfully —
                    // reset the failure set so future failures can try the
                    // full pool again.
                    self.pool.report_success();
                }
                Err(e) => {
                    // Failure recovery (spec §23): classify and report; no blind retry.
                    let reason = classify(&e);
                    // NOTE: nothing from the failed exchange is in history yet —
                    // the assistant message is only pushed AFTER a successful
                    // stream. Popping here would remove the WRONG message (the
                    // user request on iteration 1, or the last tool result after
                    // a tool round), orphaning its assistant tool_call and
                    // guaranteeing a 400 on the retry. So we never pop.
                    // --- Provider failover (Hermes credential-pool parity): if
                    // the active provider hard-failed (auth, network, rate-limit
                    // exhausted, 5xx) and another provider is available, rotate
                    // the pool and retry the iteration on the next provider —
                    // the task survives a single-provider outage.
                    let can_failover = self.pool.len() > 1;
                    if can_failover && self.pool.report_failure() {
                        let next = self.pool.name().to_string();
                        self.history.push(Message::system(format!(
                            "Provider '{next}' took over after the previous provider failed ({reason}: {e:#}). \
                             Continue the task exactly as before — the user's request is unchanged."
                        )));
                        self.set_phase(Phase::Recovering);
                        tracing::warn!("provider failover -> {next} after {reason}: {e:#}");
                        continue;
                    }
                    // No failover possible (single provider, or all failed):
                    // the task is blocked, not silently dead.
                    self.set_phase(Phase::Blocked);
                    outcome.finished = false;
                    outcome.blocked_reason = Some(format!("{reason}: {e:#}"));
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
                let empty_response = content.trim().is_empty();
                // --- Question-to-user gate (CAP OFF): wait for the user ---
                // The model asked a clarifying question or offered a choice.
                // Do NOT auto-continue (the completion probe would read the
                // question as "not done" and nudge the model to answer its
                // own question 2-3s later). Push the question into history
                // and return needs_input so the caller pauses and waits for
                // the human's answer; the answer continues the same turn.
                if !empty_response
                    && !self.config.cap_enabled
                    && is_user_question(&content)
                {
                    self.history.push(Message::assistant(
                        Some(content.clone()),
                        None,
                        if reasoning.is_empty() { None } else { Some(reasoning) },
                    ));
                    self.set_phase(Phase::AwaitingInput);
                    outcome.final_text = content;
                    outcome.needs_input = true;
                    outcome.finished = false;
                    return Ok(outcome);
                }
                // --- Empty/degenerate response guard (FIX: was "finalize empty") ---
                // A stream that produced NO content and NO tool calls is not a
                // successful completion — it's a degenerate provider/model
                // response (reasoning-only output, transient glitch, stream cut
                // after usage). Treat it like any other stall: nudge and retry,
                // bounded by the same stuck-loop guard. Only finalize when the
                // model actually produced an answer. Reasoning alone (thinking
                // without answering) is NOT a final answer — content is the
                // deliverable.
                if self.config.auto_continue
                    && no_tool_streak < 3
                    && (empty_response || (outcome.tool_calls_made > 0 && !self.check_done(&content).await))
                {
                    no_tool_streak += 1;
                    if empty_response {
                        // Don't push an empty assistant message (providers reject
                        // or models misread them). Push only the nudge.
                        self.history.push(Message::system(
                            "Your last response was empty — no text and no tool calls were \
                             produced. Re-read the conversation so far and CONTINUE the task: \
                             call the next tool or, if every part is genuinely done, write your \
                             final answer now.",
                        ));
                    } else {
                        self.history.push(Message::assistant(
                            Some(content.clone()),
                            None,
                            if reasoning.is_empty() { None } else { Some(reasoning.clone()) },
                        ));
                        self.history.push(Message::system(
                            "The task is NOT complete yet. Continue working — keep calling \
                             tools until every part of the original request is implemented and \
                             verified. Do NOT summarize or stop early. If you are genuinely \
                             blocked (missing information, impossible request), state the \
                             blocker clearly in one sentence and stop.",
                        ));
                    }
                    self.set_phase(Phase::Investigating);
                    continue;
                }
                // Streak guard hit (3 empty/stalled responses in a row): the
                // provider/model is not making progress. Fail loudly with a
                // clear blocked reason instead of silently ending with an
                // empty answer.
                if empty_response {
                    self.set_phase(Phase::Blocked);
                    outcome.finished = false;
                    outcome.blocked_reason = Some(format!(
                        "model returned {no_tool_streak} consecutive empty responses (no text, no tool calls) \
                         — the provider streamed nothing usable. Retry the turn, or check the provider ({}).",
                        self.pool.name()
                    ));
                    return Ok(outcome);
                }
                // Normal completion.
                self.history.push(Message::assistant(
                    if content.is_empty() { None } else { Some(content.clone()) },
                    None,
                    if reasoning.is_empty() { None } else { Some(reasoning) },
                ));
                self.set_phase(Phase::Complete);
                outcome.final_text = content;
                outcome.finished = true;
                self.extract_memories(&outcome).await;
                return Ok(outcome);
            }

            // The model made tool calls: it's making progress — reset the
            // no-tool streak so only CONSECUTIVE stalls count.
            no_tool_streak = 0;

            // Tool round: dispatch all calls in parallel.
            self.set_phase(Phase::Implementing);
            outcome.tool_calls_made += valid_calls.len() as u32;
            self.history.push(Message::assistant(
                if content.is_empty() { None } else { Some(content) },
                Some(valid_calls.clone()),
                if reasoning.is_empty() { None } else { Some(reasoning) },
            ));

            let mut outcomes = Vec::with_capacity(valid_calls.len());
            // Parallel dispatch (the model emitted these calls in ONE message,
            // asserting independence — OpenAI/Claude/Hermes semantics). The
            // old loop ran them sequentially, serializing latency on every
            // multi-tool turn. A concurrency cap keeps the machine from
            // thrashing when the model emits a large batch of shell commands.
            const MAX_PARALLEL_TOOLS: usize = 6;
            // Long-running tools (spawn/delegation) get a dedicated, much
            // larger timeout instead of the generic per-tool cap — a real
            // multi-agent delegation can legitimately exceed 300s and must
            // NOT be killed mid-flight (children keep running orphaned, the
            // main agent abandons the results and "falls back" to doing
            // everything itself). Children are individually bounded by their
            // own AGENT_TIMEOUT, so nothing can hang forever.
            const LONG_TOOL_TIMEOUT: Duration = Duration::from_secs(1800);
            let tool_timeout = self.config.tool_timeout;
            let tool_names = self.tools.names().join(", ");
            let mut results: Vec<Option<ToolOutcome>> = vec![None; valid_calls.len()];
            let mut set = tokio::task::JoinSet::new();
            let mut next: usize = 0;
            // Fill the initial window, then keep the window full as tasks
            // complete — bounded concurrency, all calls started ASAP.
            while next < valid_calls.len() || !set.is_empty() {
                while set.len() < MAX_PARALLEL_TOOLS && next < valid_calls.len() {
                    let idx = next;
                    let call = &valid_calls[idx];
                    let tool = self.tools.get(&call.name);
                    let call_name = call.name.clone();
                    let tool_names = tool_names.clone();
                    let eff_timeout = if tool.as_ref().map(|t| t.long_running()).unwrap_or(false) {
                        LONG_TOOL_TIMEOUT
                    } else {
                        tool_timeout
                    };
                    let args: Value = serde_json::from_str(&call.arguments)
                        .unwrap_or_else(|_| serde_json::json!({"error": format!("unparseable args: {}", call.arguments)}));
                    set.spawn(async move {
                        let outcome = match tool {
                            Some(t) => {
                                let res = tokio::time::timeout(eff_timeout, t.execute(args)).await;
                                match res {
                                    Ok(r) => r,
                                    Err(_) => ToolOutcome {
                                        call_id: String::new(),
                                        name: call_name,
                                        output: format!("ERROR: tool timeout after {}s", eff_timeout.as_secs()),
                                        ok: false,
                                        elapsed_ms: eff_timeout.as_millis() as u64,
                                    },
                                }
                            }
                            None => ToolOutcome {
                                call_id: String::new(),
                                name: call_name.clone(),
                                output: format!("ERROR: unknown tool {:?}. Available: {tool_names}", call_name),
                                ok: false,
                                elapsed_ms: 0,
                            },
                        };
                        (idx, outcome)
                    });
                    next += 1;
                }
                if let Some(Ok((idx, outcome))) = set.join_next().await {
                    results[idx] = Some(outcome);
                }
            }
            // Live status: show every running tool at once, then clear.
            if let Ok(mut l) = self.live.try_lock() {
                l.active_tools = valid_calls.iter().map(|c| c.name.clone()).collect();
                l.phase = Phase::Implementing;
            }
            for (call, slot) in valid_calls.iter().zip(results.iter()) {
                let mut outcome = slot.clone().unwrap_or_else(|| ToolOutcome {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    output: "ERROR: tool task failed to complete".into(),
                    ok: false,
                    elapsed_ms: 0,
                });
                // Stamp the real call id — tools don't know it, and providers
                // reject tool messages with an empty/missing tool_call_id.
                outcome.call_id = call.id.clone();
                debug!("tool {} -> ok={} {}ms", call.name, outcome.ok, outcome.elapsed_ms);
                on_tool(&outcome);
                outcomes.push(outcome);
            }
            if let Ok(mut l) = self.live.try_lock() {
                l.active_tools.clear();
            }

            // If every tool failed with "unknown tool", disable tools to avoid a retry loop.
            if !outcomes.is_empty() && outcomes.iter().all(|o| !o.ok) && outcomes.iter().any(|o| o.output.contains("unknown tool")) {
                tools_active = false;
                defs.clear();
                self.set_phase(Phase::Recovering);
            }

            // Grow the Smart Tool Selection on demand: if the model called a
            // tool we didn't expose this round, add its def so it stays
            // available next iteration (OpenJarvis ROUTE + isair grow-set).
            if self.config.tool_select && tools_active {
                for c in &valid_calls {
                    if defs.iter().any(|d| d.name == c.name) {
                        continue;
                    }
                    if let Some(tool) = self.tools.get(&c.name) {
                        defs.push(tool.def());
                    }
                }
            }

            for o in &outcomes {
                let output = truncate_tool_output(o, self.config.max_tool_output_chars);
                self.history.push(Message::tool(&o.call_id, &o.name, &output));
            }
        }

        self.set_phase(Phase::Recovering);

        // --- Graceful budget exhaustion (beyond Hermes: we still deliver a
        // real final answer from partial progress, and record WHY we stopped) ---
        let reason = exhaustion.unwrap_or_else(|| format!("iteration budget ({capped} iters)"));
        // Surface WHY it stopped in the live status so the UI can show it.
        if let Ok(mut l) = self.live.try_lock() {
            l.phase = Phase::Recovering;
            l.note = format!("stopped: {reason} — raise max_iterations in config (0 = unlimited)");
        }
        self.history.push(Message::system(format!(
            "Reached the {reason}. Do NOT call any tools. Provide your best final \
             answer now, summarizing what you found and completed so far."
        )));
        let defs: Vec<byteai_types::ToolDef> = Vec::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        let wire_history = self.sanitized_history();
        // The wrap-up call must not be able to hang the turn: a reasoning
        // model can sit on reasoning_content for a long time, and a dead
        // stream yields an empty "final answer". Bound it with the run budget
        // remainder (or a sane 120s default) and, on empty, fail loudly.
        let wrap_budget = self
            .config
            .run_budget_seconds
            .map(|b| {
                let used = started.elapsed().as_secs();
                b.saturating_sub(used).max(10)
            })
            .unwrap_or(120);
        let mut saw_any = false;
        let wrap_result = tokio::time::timeout(
            Duration::from_secs(wrap_budget),
            self.pool
                .client()
                .chat_stream(self.pool.model(), &wire_history, &defs, self.config.max_tokens, |ev| {
                    match ev {
                        StreamEvent::Content(c) => {
                            saw_any = true;
                            content.push_str(&c);
                            on_text(&c);
                        }
                        StreamEvent::Reasoning(r) => {
                            reasoning.push_str(&r);
                        }
                        _ => {}
                    }
                }),
        )
        .await;
        match wrap_result {
            Ok(Ok(())) if saw_any || !content.trim().is_empty() || !reasoning.trim().is_empty() => {}
            Ok(Ok(())) => {
                // Empty stream at wrap-up: still fail loudly, not silently empty.
                self.history.pop();
                self.set_phase(Phase::Blocked);
                outcome.finished = false;
                outcome.blocked_reason = Some(format!("{reason}: wrap-up stream returned no content"));
                outcome.exhausted = true;
                outcome.exhausted_reason = Some(reason);
                return Ok(outcome);
            }
            Ok(Err(e)) => {
                // Drop the wrap-up system message so the history stays clean.
                self.history.pop();
                self.set_phase(Phase::Blocked);
                outcome.finished = false;
                outcome.blocked_reason = Some(format!("{reason}: wrap-up failed: {e:#}"));
                outcome.exhausted = true;
                outcome.exhausted_reason = Some(reason);
                return Ok(outcome);
            }
            Err(_) => {
                // Wrap-up timed out — fail loudly instead of delivering an
                // empty "answer".
                self.history.pop();
                self.set_phase(Phase::Blocked);
                outcome.finished = false;
                outcome.blocked_reason = Some(format!("{reason}: wrap-up timed out after {wrap_budget}s"));
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
            self.set_phase(Phase::Complete);
            outcome.final_text = content;
            outcome.finished = true;
        } else {
            self.set_phase(Phase::Blocked);
            outcome.finished = false;
            outcome.blocked_reason = Some(format!("{reason}: no final answer produced"));
        }
        outcome.exhausted = true;
        outcome.exhausted_reason = Some(reason);
        // mem0-style: auto-extract durable facts from this conversation turn
        // and persist them so the agent remembers across sessions.
        self.extract_memories(&outcome).await;
        // TencentDB Agent Memory: capture this turn's dialogue (L0) so the
        // team memory hub's L1-L3 pipeline can distill durable facts.
        self.capture_tdai_turn(user_input, &outcome.final_text).await;
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
        let sanitized = sanitize::sanitize(&[msg]);
        // The extraction call MUST NOT be able to hang the turn: a slow or
        // stalled provider would otherwise block for the full 600s reqwest
        // timeout AFTER the task is already complete — the user sees the
        // agent "stuck" with the answer already delivered. Bound it to 10s
        // and treat timeout/failure as "no memories this turn" (best-effort).
        let fut = self.pool.client().chat(self.pool.model(), &sanitized, &[], Some(256));
        let (text, _) = match tokio::time::timeout(Duration::from_secs(10), fut).await {
            Ok(Ok((t, _, _))) if !t.trim().is_empty() => (t.trim().to_string(), true),
            _ => return,
        };
        // Save each line as a memory entity (classic store) AND as an L1
        // atomic in the native memory hub (TencentDB Agent Memory model).
        let mut mem = match byteai_memory::Memory::open(&self.data_dir.join("memory")) {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut hub = byteai_memory::hub::MemoryHub::open(&self.data_dir.join("memory")).ok();
        for line in text.lines() {
            let line = line.trim().trim_start_matches(['-', '*', ' ']).trim();
            if line.is_empty() {
                continue;
            }
            let title = line.chars().take(60).collect::<String>();
            let _ = mem.upsert(byteai_memory::Kind::Entity, &title, line, &[String::from("auto")], None);
            if let Some(h) = hub.as_mut() {
                // Classify: persona (about the user) vs instruction (about
                // how to behave) vs episodic (anything else).
                let lower = line.to_lowercase();
                let mem_type = if lower.contains("prefer") || lower.contains("likes") || lower.contains("user")
                    || lower.contains("wants") || lower.contains("uses") {
                    "persona"
                } else if lower.contains("always") || lower.contains("never") || lower.contains("must")
                    || lower.contains("should") || lower.contains("remember to") {
                    "instruction"
                } else {
                    "episodic"
                };
                let _ = h.atomic_write(mem_type, line, Some("auto-extracted"));
            }
        }
    }

    /// Hermes-parity context compression: when the estimated history exceeds
    /// the budget, use the aux model to summarize the middle of the
    /// conversation while protecting the system prompt (head) and the last
    /// ~6 turns (tail). Falls back to plain dropping if the provider is
    /// unavailable — the task never stalls on a compression failure.
    ///
    /// Tool-pair safety: a `tool` result message MUST stay adjacent to its
    /// assistant `tool_calls` message (OpenAI-style providers reject a tool
    /// role with no preceding tool_call). The tail boundary is therefore
    /// pushed FORWARD past any leading tool results whose assistant call got
    /// compressed away, so we never send an orphaned tool message to the
    /// provider (which previously caused the turn to stall with 400 errors
    /// after a few minutes of work).
    async fn enforce_budget(&mut self) {
        let est = estimate_tokens(&self.history);
        if est <= self.config.context_budget_tokens {
            return;
        }
        // Protect: system prompt (index 0) + last ~6 turns.
        let keep = 6u32;
        let n = self.history.len();
        if n <= (keep as usize + 1) {
            return; // too small to compress meaningfully
        }
        let mut tail_start = n.saturating_sub(keep as usize);
        // Tool-pair safety: advance past orphaned tool results at the head of
        // the kept tail. If history[tail_start] is a `tool` message, its
        // assistant tool_calls message is at tail_start-1 — which is being
        // compressed away. Keeping the tool result would send an orphaned
        // `tool` role to the provider → 400 → stuck. Drop it instead.
        while tail_start < n && self.history[tail_start].role == Role::Tool {
            tail_start += 1;
        }
        if tail_start <= 1 {
            return; // nothing meaningful left to keep
        }
        let middle: Vec<Message> = self.history[1..tail_start].to_vec();
        let middle_text: String = middle
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        if middle_text.len() < 200 {
            // Too little to summarize — just drop the middle.
            self.history.drain(1..tail_start);
            return;
        }
        // Attempt aux-model summarization.
        let summary = {
            let prompt = format!(
                "Summarize the following conversation history in 2-3 concise sentences, \
                 keeping all decisions, requirements, file paths, and configuration values. \
                 This replaces the full history — everything important must be preserved.\n\n{middle_text}"
            );
            let msg = Message::user(&prompt);
            let sanitized = sanitize::sanitize(&[msg]);
            // Use the aux model via the active provider; bounded to 5s + 300 tokens.
            let fut = self.pool.client().chat(self.pool.model(), &sanitized, &[], Some(300));
            match tokio::time::timeout(Duration::from_secs(5), fut).await {
                Ok(Ok((t, _, _))) if !t.trim().is_empty() => Some(t.trim().to_string()),
                _ => None,
            }
        };
        match summary {
            Some(s) => {
                // Replace the middle with a compact system summary message.
                let compressed = Message::system(format!(
                    "[Compressed summary of earlier conversation ({est} tokens → summary):\n{s}\n]"
                ));
                self.history.drain(1..tail_start);
                self.history.insert(1, compressed);
            }
            None => {
                // Fallback: plain drop the middle (same as the old behavior).
                self.history.drain(1..tail_start);
            }
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

/// Cap how much of a tool result goes into the model context. Huge outputs
/// (e.g. the `skills` listing on a machine with thousands of skills) blow the
/// context budget and the provider window, which stalls the whole turn. Keep
/// the head (where the meaningful content usually is) plus a small tail, and
/// tell the model the full output was truncated and how big it was.
pub fn truncate_tool_output(o: &ToolOutcome, max_chars: usize) -> String {
    let raw = &o.output;
    if raw.chars().count() <= max_chars {
        return raw.clone();
    }
    let head: String = raw.chars().take(max_chars * 2 / 3).collect();
    let tail: String = raw.chars().skip(raw.chars().count().saturating_sub(max_chars / 3)).collect();
    format!(
        "[tool {name} output TRUNCATED: full result was {total} chars, showing first {head_len} + last {tail_len}]\n\n{head}\n\n... [{mid} chars omitted] ...\n\n{tail}\n\n[use the tool again with a narrower query if you need details]",
        name = o.name,
        total = raw.chars().count(),
        head_len = head.chars().count(),
        tail_len = tail.chars().count(),
        mid = raw.chars().count().saturating_sub(head.chars().count() + tail.chars().count()),
    )
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

/// Parse the completion-gate verdict: `true` = DONE (finish the turn),
/// `false` = CONTINUE (keep working). Reads only the FIRST word so trailing
/// reasoning or the "NOT DONE…" phrasing can't confuse it.
fn is_done_verdict(text: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_ascii_alphabetic())
        .to_uppercase();
    first == "DONE"
}

/// Detect whether a no-tool response is a QUESTION TO THE USER (a
/// clarification or a choice the agent wants the human to make) rather than
/// a final answer or a status report.
///
/// Why this exists: the auto-continue gate's completion probe reads ANY
/// no-tool response against "is the ORIGINAL task fully complete?" — a
/// clarifying question is correctly "not done", so the probe returns
/// CONTINUE and the nudge tells the model to keep working. The model then
/// answers its own question 2–3 seconds later and plows ahead — the
/// "asks then auto-answers" bug. When CAP is off, a detected question
/// instead PAUSES the turn and returns `needs_input` so the caller waits.
///
/// Heuristic: a trailing `?` is the strong signal (works regardless of
/// length — a final answer ending in an offer/question should wait for the
/// user's choice). Without `?`, only SHORT responses count, but the phrase
/// list is deliberately broad because models phrase requests-for-input as
/// imperatives too ("Tell me what you're working on", "Please provide…")
/// which carry no question mark. The length cap prevents a long report that
/// happens to contain "should i" from being misread as a question.
fn is_user_question(content: &str) -> bool {
    let t = content.trim();
    if t.is_empty() {
        return false;
    }
    if t.ends_with('?') {
        return true;
    }
    if t.chars().count() >= 600 {
        return false;
    }
    let lower = t.to_lowercase();
    const ASKS: &[&str] = &[
        // direct question phrases
        "what would you like",
        "which would you prefer",
        "would you prefer",
        "which option",
        "which one",
        "what's your preference",
        "what is your preference",
        "any preference",
        "please confirm",
        "should i proceed",
        "shall i",
        "should i",
        "may i",
        "is that ok",
        "is that okay",
        "would you like me to",
        "do you want me to",
        "do you want to",
        "would you like",
        // imperative requests for user input (no '?')
        "tell me what",
        "tell me more",
        "tell me about",
        "let me know",
        "let me know which",
        "please let me know",
        "please provide",
        "please share",
        "please give",
        "please tell me",
        "please advise",
        "please choose",
        "please select",
        "please explain",
        "please describe",
        "please clarify",
        "provide me",
        "give me more",
        "could you please",
        "can you provide",
        "can you give",
        "could you give",
        "i need you to",
        "i'm going to need",
        "what are the options",
        "what are you working on",
        "what is the project",
        "what's the project",
        "what's the context",
        "what is the context",
        "explain what",
        "describe what",
        "more information",
        "more context",
        "waiting for your",
        "your input",
        "your decision",
        "what do you want",
        "what do you need",
    ];
    ASKS.iter().any(|a| lower.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteai_memory::Kind;
    use byteai_provider::Client;
    use byteai_tools::Registry;
    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_core_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_agent(data_dir: &std::path::Path) -> Agent {
        let client = Client::new("http://127.0.0.1:1/v1", "test").unwrap();
        let pool = ProviderPool::single("test", client, "test-model");
        let tools = Registry::builtins(&byteai_tools::ToolContext::new(data_dir.to_path_buf()));
        let cfg = AgentConfig::default();
        Agent::new(pool, cfg, tools, data_dir.to_path_buf())
    }

    #[test]
    fn live_status_shared_arc_propagates_phase() {
        // The TUI clones `agent.live` and polls it every frame. Verify that
        // phase changes made inside the agent (via set_phase) are visible
        // through that shared clone — this is the mechanism behind the live
        // status row updating while a turn runs.
        let dir = tmp_dir("live_shared");
        let mut agent = make_agent(&dir);

        // What a TUI would do: clone the Arc once and hold it.
        let ui = agent.live.clone();

        // Initial state: Understanding.
        assert_eq!(ui.lock().unwrap().phase, Phase::Understanding);

        // Agent flips phases; UI clone must see each one immediately.
        agent.set_phase(Phase::Investigating);
        assert_eq!(ui.lock().unwrap().phase, Phase::Investigating);
        agent.set_phase(Phase::Implementing);
        assert_eq!(ui.lock().unwrap().phase, Phase::Implementing);
        agent.set_phase(Phase::Verifying);
        assert_eq!(ui.lock().unwrap().phase, Phase::Verifying);
        agent.set_phase(Phase::Complete);
        assert_eq!(ui.lock().unwrap().phase, Phase::Complete);

        // line() renders the phase label, active tools, and note.
        {
            let mut l = ui.lock().unwrap();
            l.active_tools = vec!["shell".into()];
            l.iterations = 3;
            l.iter_cap = 300;
            l.note = "test note".into();
        }
        let line = ui.lock().unwrap().line(0);
        assert!(line.contains("IMPLEMENTING") || line.contains("COMPLETE") || line.contains("VERIFYING"), "line(): {line}");
        assert!(line.contains("shell"), "active tool in line(): {line}");
        assert!(line.contains("3/300"), "iter/cap in line(): {line}");
        assert!(line.contains("test note"), "note in line(): {line}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_status_line_shows_each_phase() {
        for (phase, kw) in [
            (Phase::Understanding, "UNDERSTANDING"),
            (Phase::Investigating, "INVESTIGATING"),
            (Phase::Implementing, "IMPLEMENTING"),
            (Phase::Verifying, "VERIFYING"),
            (Phase::Recovering, "RECOVERING"),
            (Phase::Reviewing, "REVIEWING"),
            (Phase::Complete, "COMPLETE"),
            (Phase::AwaitingInput, "AWAITING_INPUT"),
            (Phase::Blocked, "BLOCKED"),
        ] {
            let ls = LiveStatus { phase, ..LiveStatus::default() };
            let line = ls.line(0);
            assert!(line.to_uppercase().contains(kw), "phase {phase:?} -> {line}");
        }
    }

    #[test]
    fn question_detection_heuristic() {
        // Trailing '?' is the strong signal — regardless of length.
        assert!(is_user_question("Should I proceed?"));
        assert!(is_user_question("Which option do you prefer for the UI — dark or light?"));
        assert!(is_user_question("The tests pass. Want me to also add integration tests?"));
        assert!(is_user_question("Do you want me to use av1 or h264?"));

        // Phrase signals without '?' only count on SHORT responses.
        assert!(is_user_question("Please choose which database to use"));
        assert!(is_user_question("what would you like to do next"));
        assert!(is_user_question("Let me know which one you prefer"));

        // Non-questions must NOT be misread.
        assert!(!is_user_question(""));
        assert!(!is_user_question("All three checks completed successfully."));
        assert!(!is_user_question("I fixed the bug and verified the fix."));
        assert!(!is_user_question("Done."));
        // Long report containing "should i" incidentally — not a question.
        // Must be > 600 chars to bypass the length cap.
        assert!(!is_user_question(
            "I refactored the module. I should mention that I also updated the docs. \
             The refactor removed 200 lines and the tests all pass. The build is green \
             and the benchmark improved by 15%. I should also note the config migration \
             I did as part of this change and how it affects the deployment pipeline. \
             Here is the full summary of everything that changed across all the files. \
             I should also mention that I updated the CI configuration to run the new \
             benchmarks as part of the pipeline. The CI now runs lint, test, and bench \
             stages in parallel and reports the results to the project dashboard. \
             Additionally I should note that the documentation was updated to reflect \
             the new API surface and the migration guide was published. This completes \
             the full refactor of the module with no breaking changes to the public API."
        ));
        // Imperative requests must be caught (no '?').
        assert!(is_user_question("Tell me what you're working on and I'll plan the best approach."));
        assert!(is_user_question("Please provide more context about the project."));
        assert!(is_user_question("Let me know which approach you prefer."));
        assert!(is_user_question("I need you to specify the requirements before I can proceed."));
    }

    #[test]
    fn cap_defaults_off_and_toggles() {
        let cfg = AgentConfig::default();
        assert!(!cfg.cap_enabled, "CAP must default to OFF (wait for user)");
    }

    #[test]
    fn inject_memories_prepends_relevant_block() {
        let dir = tmp_dir("inject");
        // Seed one memory entity about "rust async".
        let mut mem = byteai_memory::Memory::open(&dir.join("memory")).unwrap();
        mem.upsert(Kind::Entity, "rust async", "user prefers async rust", &[], None).unwrap();
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

    /// Native memory hub injection: seed L3 persona + L2 scenario + L1 atomic
    /// in byteai's own SQLite hub, then verify they land in the system prompt.
    #[tokio::test]
    async fn inject_tdai_memory_pulls_persona_and_scenarios() {
        let dir = tmp_dir("inject_tdai");
        // Seed the native hub (same data_dir the agent uses).
        {
            let mut hub = byteai_memory::hub::MemoryHub::open(&dir.join("memory")).unwrap();
            hub.core_write("I am ByteAi, a fast autonomous coding agent").unwrap();
            hub.scenario_write("prefs.md", "# Preferences\nlocal-first + 60fps", Some("coding prefs")).unwrap();
            hub.atomic_write("persona", "user prefers local-first architecture", Some("auto")).unwrap();
        }
        let mut agent = make_agent(&dir);
        agent.config.tdai.enabled = true;
        agent.with_system_prompt();
        agent.inject_tdai_memory("local-first").await;
        let sys = agent.history[0].content.clone().unwrap_or_default();
        assert!(sys.contains("[Memory L3 core persona]"), "L3 persona missing from system prompt");
        assert!(sys.contains("[Memory L2 scenarios]"), "L2 scenarios missing from system prompt");
        assert!(sys.contains("[Memory L1 atomics]"), "L1 atomics missing from system prompt");
        assert!(sys.contains("ByteAi"), "persona content not injected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Injection must be a silent no-op when disabled.
    #[tokio::test]
    async fn inject_tdai_memory_disabled_is_noop() {
        let dir = tmp_dir("inject_tdai_off");
        let mut agent = make_agent(&dir);
        agent.config.tdai.enabled = false;
        agent.with_system_prompt();
        agent.inject_tdai_memory("anything").await;
        let sys = agent.history[0].content.clone().unwrap_or_default();
        assert!(!sys.contains("[TDAI"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_status_line_renders_phase_and_iterations() {
        // Phase label + spinner frame + iteration count + cap.
        let mut s = LiveStatus {
            iterations: 3,
            iter_cap: 20,
            phase: Phase::Implementing,
            active_tools: vec!["websearch".into()],
            ..LiveStatus::default()
        };
        let line = s.line(0);
        assert!(line.contains("IMPLEMENTING"), "line: {line}");
        assert!(line.contains("websearch"), "line: {line}");
        assert!(line.contains("3/20"), "line: {line}");
        // Different phases render different icons (per-phase status style).
        s.phase = Phase::Understanding;
        let think = s.line(0);
        s.phase = Phase::Verifying;
        let verify = s.line(0);
        assert_ne!(think, verify, "phases must render differently");
        assert!(think.contains("UNDERSTANDING"));
        assert!(verify.contains("VERIFYING"));
    }

    #[test]
    fn truncate_tool_output_caps_huge_results() {
        let big = "x".repeat(200_000);
        let o = ToolOutcome {
            call_id: "c1".into(),
            name: "skills".into(),
            output: big.clone(),
            ok: true,
            elapsed_ms: 1,
        };
        let capped = truncate_tool_output(&o, 50_000);
        assert!(capped.len() < big.len(), "must be smaller than the raw output");
        assert!(capped.contains("TRUNCATED"), "note present");
        assert!(capped.contains("200000 chars"), "reports total size");
        // Small outputs pass through untouched.
        let small = ToolOutcome { call_id: "c2".into(), name: "echo".into(), output: "hello".into(), ok: true, elapsed_ms: 1 };
        assert_eq!(truncate_tool_output(&small, 50_000), "hello");
    }

    #[test]
    fn interrupted_turn_dangling_user_message_is_replaced_not_duplicated() {
        // After ESC interrupts a turn mid-stream, the agent's history ends
        // with a User message that has NO assistant reply (the aborted run
        // pushed the user input but never completed). The next run() must
        // drop that dangling user message before pushing the new one, so the
        // provider never sees two consecutive user turns.
        let dir = tmp_dir("interrupt_dedup");
        let mut agent = make_agent(&dir);
        agent.with_system_prompt();
        agent.inject_memories("q1");

        // Simulate an interrupted turn: user msg pushed, run aborted before
        // any assistant reply (exactly what happens on ESC mid-stream).
        agent.history.push(Message::user("q1"));
        assert!(matches!(agent.history.last(), Some(Message { role: Role::User, .. })));

        // Now the user asks a follow-up ("continue"). run() should replace
        // the dangling user msg, not stack a second one on top.
        agent.inject_memories("continue");
        if let Some(Message { role: Role::User, .. }) = agent.history.last() {
            agent.history.pop();
        }
        agent.history.push(Message::user("continue"));

        // Exactly one trailing user message; no two-in-a-row.
        let users = agent.history.iter().filter(|m| m.role == Role::User).count();
        assert_eq!(users, 1, "dangling user msg replaced, not duplicated");
        assert!(matches!(agent.history.last(), Some(Message { role: Role::User, .. })));
        assert_eq!(agent.history.last().unwrap().content.as_deref(), Some("continue"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn done_verdict_parses_first_word_only() {
        // The completion gate must treat only a leading DONE as completion;
        // reasoning, "NOT DONE", or anything else means CONTINUE.
        assert!(is_done_verdict("DONE"));
        assert!(is_done_verdict("DONE the task is complete"));
        assert!(is_done_verdict("  done. "));
        assert!(!is_done_verdict("CONTINUE"));
        assert!(!is_done_verdict("CONTINUE: 3 fixes remain"));
        assert!(!is_done_verdict("NOT DONE"));
        assert!(!is_done_verdict("not done yet"));
        assert!(!is_done_verdict(""));
        assert!(!is_done_verdict("Still working on the kanban fix"));
    }

    #[test]
    fn strip_injected_blocks_resets_system_prompt_across_turns() {
        // Multi-turn sessions call run() repeatedly; each call re-injects
        // memory/skills blocks. They must be STRIPPED before re-injection so
        // the system prompt does not grow unboundedly across turns.
        let dir = tmp_dir("strip_inject");
        let mut agent = make_agent(&dir);
        agent.config.tdai.enabled = true;
        agent.with_system_prompt();

        // Seed the stores so every injector has something to inject.
        {
            let mut mem = byteai_memory::Memory::open(&dir.join("memory")).unwrap();
            mem.upsert(Kind::Entity, "rust async", "user prefers async rust", &[], None).unwrap();
            drop(mem);
            let skills_root = dir.join("skills");
            std::fs::create_dir_all(&skills_root).unwrap();
            std::fs::create_dir_all(skills_root.join("rust-functions")).unwrap();
            std::fs::write(
                skills_root.join("rust-functions").join("SKILL.md"),
                "---\nname: rust-functions\ndescription: How to write a function in Rust\n---\nInstructions for writing Rust functions.\n",
            )
            .unwrap();
            let mut hub = byteai_memory::hub::MemoryHub::open(&dir.join("memory")).unwrap();
            hub.core_write("I am ByteAi, a fast autonomous coding agent").unwrap();
            drop(hub);
        }

        // First turn: inject memories + skills + tdai blocks (simulating the
        // run() preamble).
        agent.inject_memories("tell me about async rust");
        agent.inject_skills("write a function");
        tokio::runtime::Runtime::new().unwrap().block_on(agent.inject_tdai_memory("async"));
        let after_turn1 = agent.history[0].content.clone().unwrap_or_default();
        assert!(after_turn1.contains("Relevant prior knowledge"));
        assert!(after_turn1.contains("Relevant skills"));
        assert!(after_turn1.contains("[Memory L3 core persona]"));

        // Second turn: strip + re-inject. The system prompt must NOT contain
        // two copies of each block.
        agent.strip_injected_blocks();
        agent.inject_memories("tell me about async rust");
        agent.inject_skills("write a function");
        tokio::runtime::Runtime::new().unwrap().block_on(agent.inject_tdai_memory("async"));
        let after_turn2 = agent.history[0].content.clone().unwrap_or_default();

        assert_eq!(
            after_turn2.matches("Relevant prior knowledge").count(),
            1,
            "system prompt must have exactly one memory block after re-inject"
        );
        assert_eq!(
            after_turn2.matches("Relevant skills").count(),
            1,
            "system prompt must have exactly one skills block after re-inject"
        );
        assert_eq!(
            after_turn2.matches("[Memory L3 core persona]").count(),
            1,
            "system prompt must have exactly one tdai block after re-inject"
        );
        // Base system prompt is preserved (not truncated away).
        assert!(after_turn2.contains("You are ByteAi"), "base prompt must survive strip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn check_done_probe_failure_treats_as_continue() {
        // The probe targets an unreachable provider, so it fails. That must
        // NOT be treated as DONE: a transient probe error (rate limit,
        // network blip) must never falsely end a task that still has work
        // remaining. The stuck-loop guard in run() (no_tool_streak < 3)
        // bounds how many CONTINUEs we retry, so the turn can't hang either.
        let dir = tmp_dir("check_done_fail");
        let agent = make_agent(&dir); // Client points at 127.0.0.1:1
        let result = agent.check_done("some partial output").await;
        assert!(!result, "probe failure must mean CONTINUE, not DONE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
