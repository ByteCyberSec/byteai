//! ByteAi TUI — jcode-style interaction, unique ByteAi layout.
//! Mirrors jcode's command surface (/help, /model, /clear, /save, /usage,
//! /session, /keys, /config, /subagent, /swarm, ...) and keybindings
//! (Ctrl+Shift+K/J scroll, Ctrl+Tab model switch, Alt+U/D page, Ctrl+C
//! interrupt), with a unique ByteAi header/banner/status layout.
//!
//! Responsiveness model (jcode-style): the agent turn runs in a background
//! tokio task; text tokens and tool events stream back through an mpsc
//! channel and are rendered live. While busy, the header shows a spinner
//! and an elapsed-seconds counter. Esc / Ctrl+C abort the in-flight task.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use byteai_core::Agent;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, Paragraph};
use ratatui::{Frame, Terminal};
use crate::toolcards;

const MAX_LOG: usize = 3000;

/// All supported /commands (name, description). Used by the command palette
/// that appears when the user types `/`.
const COMMANDS: &[(&str, &str)] = &[
    ("help", "this message"),
    ("status", "session: model, provider, tokens, context"),
    ("new", "start a fresh session"),
    ("retry", "resend the last message"),
    ("undo", "back up a turn: /undo [N]"),
    ("compress", "summarize + compact the context"),
    ("title", "name/save the session: /title [name]"),
    ("model", "switch model — scroll-pick or /model <name>"),
    ("provider", "switch provider — scroll-pick or /provider <name>"),
    ("addprovider", "add provider+model: /addprovider name url model [key]"),
    ("reload", "reload config.toml into the session"),
    ("tools", "tool status: /tools [auto|all]"),
    ("clear", "clear conversation"),
    ("save", "save session: /save [name]"),
    ("usage", "show token usage (/usage reset)"),
    ("session", "resume a saved session — scroll-pick"),
    ("resume", "resume by name: /resume <id>"),
    ("config", "show config path"),
    ("keys", "show keybindings"),
    ("version", "show version"),
    ("copy", "copy last response to clipboard"),
    ("diff", "git working-tree summary"),
    ("route", "route a task to the best model"),
    ("council", "multi-model deliberation vote"),
    ("govern", "constitutional guardrail check"),
    ("gates", "acceptance ledger: status/run/reverify/create"),
    ("cap", "toggle CAP (Coding Auto-Pilot): ON=autonomous, OFF=wait for your answers"),
    ("ideas", "evidence-based idea discovery: /ideas <focus>"),
    ("github", "capability discovery: /github <target> <query>"),
    ("goal", "durable session goal: /goal set <text> | get | clear | complete"),
    ("terminal", "persistent shell: /terminal create|list|run <id> <cmd>|close <id>"),
    ("feedback", "record feedback: /feedback remark <text> | rate <id> <1-5> | stats"),
    ("setup", "interactive setup wizard: /setup (providers, models, skills, tools)"),
    ("subagent", "spawn parallel subagents"),
    ("swarm", "spawn 3-way swarm"),
    ("dan", "Dan methodology: /dan [topic] | query <t> | apply <task> | help"),
    ("quit", "exit"),
];

/// Messages streamed from the background agent task to the UI loop.
enum TurnMsg {
    Text(String),
    Tool(byteai_types::ToolOutcome),
    Done(byteai_types::AgentOutcome),
    Err(String),
}

#[derive(Clone)]
enum LogEntry {
    Text { content: String, style: Style },
    /// Streamed assistant text; chunks coalesce onto the current line so
    /// tokens don't wrap onto separate lines (jcode-style).
    Assistant(String),
    /// A tool outcome rendered as an interactive "activity card": icon +
    /// color-coded status + humanized time, with the full output kept so the
    /// card can be expanded/collapsed in place (Enter with the card focused).
    ToolCard {
        id: u64,
        name: String,
        ok: bool,
        elapsed_ms: u64,
        output: String,
        truncated: bool,
    },
    Meta(String),
}

struct App {
    log: Vec<LogEntry>,
    input: String,
    is_command: bool,
    scroll: usize,
    /// When true, new content auto-scrolls to the bottom. Set false on
    /// manual scroll-up, reset true on scroll-down to bottom or new user msg.
    follow_bottom: bool,
    /// Total scrollable rows above the bottom (recomputed each frame in
    /// draw_chat). Manual scroll-up is capped by this so the user can reach
    /// the very top of the transcript — NOT by log entry count.
    max_scroll: usize,
    /// Prompts queued while a turn is still running; auto-sent in order
    /// when the current turn finishes so questions are never dropped.
    pending_queue: Vec<String>,
    model: String,
    provider: String,
    tools_count: usize,
    skills_count: usize,
    last_tokens: u64,
    last_iters: u32,
    last_tools: u32,
    /// Per-turn iteration cap (0 = unlimited), shown in the footer.
    iter_cap: u32,
    models: Vec<String>,
    model_idx: usize,
    turn_task: Option<tokio::task::JoinHandle<()>>,
    turn_rx: Option<tokio::sync::mpsc::UnboundedReceiver<TurnMsg>>,
    /// Channel for auto-review results (non-blocking, spawned per heavy turn).
    review_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    review_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// WARN/ERROR notifications captured from the tracing pipeline (provider
    /// retries, transient errors, …). Drained into the chat log as Meta
    /// entries so they surface after the answer — never written to stderr,
    /// which would land at the terminal cursor and corrupt the typing box.
    notify_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    /// Live activity status (phase, active tools, iteration progress) shared
    /// with the agent core — polled every frame for the Hermes-style status line.
    live: Option<Arc<std::sync::Mutex<byteai_core::LiveStatus>>>,
    /// The most recent completed turn outcome (consumed by run_loop for
    /// exhaustion notices + auto-review triggering).
    last_outcome: Option<byteai_types::AgentOutcome>,
    busy: bool,
    busy_since: Option<Instant>,
    /// Duration of the last completed run (seconds), shown when idle so the
    /// status never reports "finished in 0s" right after a real run.
    last_run_duration: u64,
    spinner_frame: usize,
    /// Selected command palette index (0-based, 0 = first match).
    palette_idx: usize,
    /// Interactive scroll-picker for list commands (/models, /provider,
    /// /session, /gates). When set, Up/Down/Enter/Esc drive the list.
    picker: Option<Picker>,
    /// Command awaiting one more argument (Hermes-style prompt): when set,
    /// the next Enter sends the typed text as the command's argument.
    pending_cmd: Option<String>,
    /// Label shown above the input box while a command awaits its argument.
    pending_label: Option<String>,
    /// Last command run + when (debounce: identical repeats inside 300ms are
    /// ignored — kills double-Enter/key-repeat duplicates at the source).
    last_cmd: Option<(String, std::time::Instant)>,
    /// Monotonic id source for tool cards (ids survive log trimming, so card
    /// focus/expansion stays stable even when old entries are pruned).
    next_card_id: u64,
    /// Which tool card is currently focused (its stable id). Tab / Shift+Tab
    /// cycle; Enter (with empty input) expands/collapses; Esc clears.
    card_focus: Option<u64>,
    /// Ids of tool cards that are expanded (full output shown).
    card_expanded: std::collections::HashSet<u64>,
    /// Tool calls made during the current turn (name, elapsed) — collected
    /// into the end-of-turn "ribbon" summary line.
    turn_tools: Vec<(String, u64)>,
    /// When the current turn started (ribbon duration + status timing).
    turn_start: Option<Instant>,
    /// Geometry of the chat area from the last frame — used to map a mouse
    /// click to the exact tool card under the cursor.
    last_chat_y: u16,
    last_chat_h: u16,
    last_chat_w: u16,
}

/// A scrollable pick list (Hermes-style): items with underlying values, and
/// the action to run on Enter.
struct Picker {
    title: String,
    items: Vec<String>,
    values: Vec<String>,
    sel: usize,
    action: PickAction,
}

enum PickAction {
    SetModel { provider: String },
    SwitchProvider,
    ResumeSession,
    GatesStatus,
}

impl App {
    fn new(model: String, provider: String, tools_count: usize, skills_count: usize) -> Self {
        let (review_tx, review_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        Self {
            log: Vec::new(),
            input: String::new(),
            is_command: false,
            scroll: 0,
            model,
            provider,
            tools_count,
            skills_count,
            last_tokens: 0,
            last_iters: 0,
            last_tools: 0,
            iter_cap: 0,
            models: Vec::new(),
            model_idx: 0,
            turn_task: None,
            turn_rx: None,
            review_rx: Some(review_rx),
            review_tx,
            notify_rx: None,
            live: None,
            last_outcome: None,
            busy: false,
            busy_since: None,
            last_run_duration: 0,
            spinner_frame: 0,
            palette_idx: 0,
            picker: None,
            pending_cmd: None,
            pending_label: None,
            last_cmd: None,
            follow_bottom: true,
            max_scroll: 0,
            pending_queue: Vec::new(),
            next_card_id: 0,
            card_focus: None,
            card_expanded: std::collections::HashSet::new(),
            turn_tools: Vec::new(),
            turn_start: None,
            last_chat_y: 0,
            last_chat_h: 0,
            last_chat_w: 0,
        }
    }

    fn add_user(&mut self, text: &str) {
        for line in text.lines() {
            self.log.push(LogEntry::Text {
                content: format!("❯ {line}"),
                style: Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            });
        }
        self.scroll = 0;
        self.follow_bottom = true;
    }

    /// Append one assistant output LINE — always a fresh entry, never
    /// coalesced. Used for multi-line command output (/help, /keys) so each
    /// line sits on its own row like jcode.
    fn add_assistant(&mut self, text: &str) {
        for line in text.split('\n') {
            if line.is_empty() {
                continue;
            }
            self.log.push(LogEntry::Assistant(line.to_string()));
        }
        if self.follow_bottom {
            self.scroll = 0;
        }
        while self.log.len() > MAX_LOG {
            self.log.remove(0);
        }
    }

    /// Streamed assistant token chunks: coalesce onto the current Assistant
    /// line so tokens don't wrap onto separate rows (jcode-style). Newlines
    /// inside a chunk start a new line.
    fn add_stream(&mut self, text: &str) {
        let mut first = true;
        for line in text.split('\n') {
            if first {
                first = false;
                if line.is_empty() {
                    continue;
                }
                if let Some(LogEntry::Assistant(cur)) = self.log.last_mut() {
                    cur.push_str(line);
                } else {
                    self.log.push(LogEntry::Assistant(line.to_string()));
                }
            } else {
                self.log.push(LogEntry::Assistant(line.to_string()));
            }
        }
        // Auto-follow the newest content while it streams (only if the user
        // hasn't deliberately scrolled up).
        if self.follow_bottom {
            self.scroll = 0;
        }
        while self.log.len() > MAX_LOG {
            self.log.remove(0);
        }
    }

    fn add_tool_card(&mut self, name: &str, ok: bool, elapsed_ms: u64, output: &str) {
        const MAX_OUTPUT: usize = 24 * 1024;
        let id = self.next_card_id;
        self.next_card_id += 1;
        let (truncated, full) = if output.len() > MAX_OUTPUT {
            let mut cap = output[..MAX_OUTPUT].to_string();
            // Ensure we don't split a multi-byte UTF-8 char.
            while !cap.is_char_boundary(cap.len()) {
                cap.pop();
            }
            cap.push_str("\n… (output truncated)");
            (true, cap)
        } else {
            (false, output.to_string())
        };
        self.log.push(LogEntry::ToolCard { id, name: name.to_string(), ok, elapsed_ms, output: full, truncated });
        self.turn_tools.push((name.to_string(), elapsed_ms));
        if self.follow_bottom {
            self.scroll = 0;
        }
    }

    fn add_done(&mut self, iter: u32, tools: u32, tokens: u64) {
        // Keep the stats in the status bar (footer) only — no per-answer
        // clutter in the chat stream.
        self.last_tokens = tokens;
        self.last_iters = iter;
        self.last_tools = tools;
        // Ribbon: which tools ran and how long the whole turn took.
        if !self.turn_tools.is_empty() {
            let total_ms = self
                .turn_start
                .map(|t| t.elapsed().as_millis() as u64)
                .unwrap_or(0);
            self.add_meta(format!("  {}", toolcards::ribbon(&self.turn_tools, total_ms)));
            self.turn_tools.clear();
        }
        self.turn_start = None;
        while self.log.len() > MAX_LOG {
            self.log.remove(0);
        }
    }

    fn add_error(&mut self, text: &str) {
        // Surface errors in the live-status row (via the shared LiveStatus note)
        // instead of dumping them into the chat log. Clears on next run start.
        if let Some(ref l) = self.live
            && let Ok(mut ls) = l.lock() {
                ls.note = sanitize_note(text);
            }
    }

    fn add_meta(&mut self, text: impl Into<String>) {
        self.log.push(LogEntry::Meta(text.into()));
        if self.follow_bottom {
            self.scroll = 0;
        }
    }

    /// All tool-card ids currently in the log, in order.
    fn card_ids(&self) -> Vec<u64> {
        self.log
            .iter()
            .filter_map(|e| match e {
                LogEntry::ToolCard { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Move card focus to the next/previous tool card (wraps). Focus is
    /// sticky: once a card is focused, Tab/Shift+Tab cycle through them.
    fn cycle_card_focus(&mut self, dir: i64) {
        let ids = self.card_ids();
        if ids.is_empty() {
            self.card_focus = None;
            return;
        }
        let next = match self.card_focus {
            Some(cur) => match ids.iter().position(|x| *x == cur) {
                Some(i) => ids[((i as i64 + dir).rem_euclid(ids.len() as i64)) as usize],
                None => ids[0],
            },
            None => {
                if dir >= 0 {
                    ids[0]
                } else {
                    ids[ids.len() - 1]
                }
            }
        };
        self.card_focus = Some(next);
    }

    /// Expand/collapse the focused tool card (Enter with an empty input).
    fn toggle_card(&mut self) {
        if let Some(id) = self.card_focus
            && !self.card_expanded.remove(&id)
        {
            self.card_expanded.insert(id);
        }
    }

    fn clear(&mut self) {
        self.log.clear();
        self.pending_queue.clear();
        self.scroll = 0;
        self.follow_bottom = true;
        self.card_focus = None;
        self.card_expanded.clear();
        self.turn_tools.clear();
        self.turn_start = None;
    }

    /// Drain pending events from the background agent task into the log.
    fn drain(&mut self) {
        // Auto-review results first: they arrive on their own channel and
        // must be drained even when turn_rx is None (i.e. between turns).
        if let Some(mut rrx) = self.review_rx.take() {
            while let Ok(msg) = rrx.try_recv() {
                self.add_meta(format!("  🧠 {msg}"));
            }
            self.review_rx = Some(rrx);
        }

        // Tracing notifications (WARN/ERROR from the provider/agent): surface
        // them in the chat log, never in the input box.
        if let Some(mut nrx) = self.notify_rx.take() {
            while let Ok(msg) = nrx.try_recv() {
                let msg = sanitize_note(&msg);
                self.add_meta(format!("  ⚠ {msg}"));
            }
            self.notify_rx = Some(nrx);
        }

        let mut rx = match self.turn_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        loop {
            match rx.try_recv() {
                Ok(TurnMsg::Text(t)) => self.add_stream(&t),
                Ok(TurnMsg::Tool(o)) => self.add_tool_card(&o.name, o.ok, o.elapsed_ms, &o.output),
                Ok(TurnMsg::Done(outcome)) => {
                    // The model asked the user a question (CAP off): the turn
                    // paused to wait. Show the waiting state; the user's next
                    // typed input continues the same task (the question is
                    // already in history).
                    if outcome.needs_input {
                        if let Some(ref l) = self.live
                            && let Ok(mut ls) = l.lock() {
                                ls.phase = byteai_core::Phase::AwaitingInput;
                                ls.note = "waiting for your answer — type below and Enter".into();
                            }
                        self.add_meta("  ✋ byteai is waiting for your answer — type below and press Enter to continue the task.");
                        if let Some(t) = self.busy_since.take() {
                            self.last_run_duration = t.elapsed().as_secs().max(1);
                        }
                        self.last_outcome = Some(outcome);
                        self.busy = false;
                        self.turn_task = None;
                        break;
                    }
                    self.add_done(outcome.iterations, outcome.tool_calls_made, outcome.usage.total_tokens);
                    // Surface the blocked reason (if any) in the live-status row.
                    if let Some(ref reason) = outcome.blocked_reason
                        && let Some(ref l) = self.live
                            && let Ok(mut ls) = l.lock() {
                                ls.note = sanitize_note(reason);
                            }
                    // Surface graceful budget exhaustion (final answer from
                    // partial progress) with WHY it stopped.
                    if outcome.exhausted {
                        let reason = outcome.exhausted_reason.as_deref().unwrap_or("interaction budget");
                        self.add_meta(format!("  ⚠ {reason} reached — final answer from partial progress"));
                    }
                    if let Some(t) = self.busy_since.take() {
                        self.last_run_duration = t.elapsed().as_secs().max(1);
                    }
                    self.last_outcome = Some(outcome);
                    self.busy = false;
                    self.turn_task = None;
                    // Receiver consumed: leave turn_rx = None.
                    break;
                }
                Ok(TurnMsg::Err(e)) => {
                    // Set the live status to Blocked + note the error so the
                    // status row reflects the failure (not just the chatbox).
                    if let Some(ref l) = self.live
                        && let Ok(mut ls) = l.lock() {
                            ls.phase = byteai_core::Phase::Blocked;
                            ls.note = sanitize_note(&e);
                        }
                    self.add_error(&e);
                    if let Some(t) = self.busy_since.take() {
                        self.last_run_duration = t.elapsed().as_secs().max(1);
                    }
                    self.busy = false;
                    self.turn_task = None;
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    // Keep draining next frame; put receiver back.
                    self.turn_rx = Some(rx);
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.turn_rx = None;
                    break;
                }
            }
        }
    }
}

// ── Tracing capture (WARN/ERROR → chat log, not stderr) ──────────────

/// Writer that forwards each formatted tracing line into the TUI's
/// notification channel (surfaced in the chat log), instead of stderr —
/// a stderr write in raw mode lands at the terminal cursor and corrupts
/// the input/typing box.
struct NotifyWriter(tokio::sync::mpsc::UnboundedSender<String>);

impl io::Write for NotifyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !buf.is_empty() {
            let line = String::from_utf8_lossy(buf);
            let _ = self.0.send(line.trim_end().to_string());
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NotifyMake(tokio::sync::mpsc::UnboundedSender<String>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for NotifyMake {
    type Writer = NotifyWriter;
    fn make_writer(&'a self) -> Self::Writer {
        NotifyWriter(self.0.clone())
    }
}

/// Install the TUI tracing pipeline: WARN/ERROR events are captured and
/// forwarded to the chat-log notification channel. Nothing reaches stderr.
/// Called from TUI run() — main() skips its stderr init for the TUI path,
/// so this becomes the process-wide default subscriber.
fn install_tui_tracing(tx: tokio::sync::mpsc::UnboundedSender<String>) {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,byteai=warn"));
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(NotifyMake(tx))
        .with_target(false)
        .with_ansi(false)
        .without_time();
    let _ = tracing_subscriber::registry()
        .with(layer.with_filter(filter))
        .try_init();
}

pub async fn run() -> anyhow::Result<()> {
    // The TUI owns the tracing pipeline: WARN/ERROR events (provider retries,
    // transient errors, …) are captured and surfaced in the chat log as
    // notifications — never written to stderr, which in raw mode lands at the
    // terminal cursor and corrupts the input/typing box. Installed BEFORE any
    // config/provider work so warnings from those too reach the chat log.
    let (notify_tx, notify_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    install_tui_tracing(notify_tx);

    let cfg = crate::config::load()?;
    let provider = crate::config::resolve_provider(&cfg, None, None, None);
    let model = crate::config::resolve_model(&cfg, None, &provider);
    let client = byteai_provider::Client::new(provider.base_url.clone(), provider.resolved_key())?;
    let data_dir = crate::config::data_dir();
    let lsp = Arc::new(byteai_lsp::LspRegistry::new(byteai_lsp::default_servers()));
    let mut tool_ctx = byteai_tools::ToolContext::with_lsp(data_dir.clone(), lsp);
    tool_ctx = tool_ctx
        .with_provider(client.clone(), model.clone())
        .with_limits(cfg.agent.delegation_max_iterations, cfg.agent.run_budget_seconds);
    let tools = byteai_tools::Registry::builtins(&tool_ctx);
    let tools_count = tools.names().len();
    let agent_cfg = byteai_core::AgentConfig {
        model: model.clone(),
        max_iterations: cfg.agent.max_iterations,
        run_budget_seconds: cfg.agent.run_budget_seconds.filter(|&b| b > 0),
        warn_ratio: cfg.agent.budget_warn_ratio,
        tool_timeout: std::time::Duration::from_secs(cfg.agent.tool_timeout_seconds.unwrap_or(300)),
        auto_continue: cfg.agent.auto_continue,
        cap_enabled: cfg.agent.cap_enabled,
        tool_select: cfg.agent.tool_select,
        tool_select_max: cfg.agent.tool_select_max,
        tdai: cfg.memory.to_tdai_config(),
        ..Default::default()
    };
    // Build a failover pool from all configured providers (same as CLI).
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let provider_name = provider.name.clone();
    let push_entry = |entries: &mut Vec<byteai_provider::pool::ProviderEntry>,
                      seen: &mut std::collections::HashSet<String>,
                      name: &str, p: &crate::config::ProviderEntry, mdl: &str| {
        if seen.contains(name) {
            return;
        }
        if let Ok(c) = byteai_provider::Client::new(p.base_url.clone(), p.resolved_key()) {
            entries.push(byteai_provider::pool::ProviderEntry {
                name: name.to_string(),
                client: c,
                model: mdl.to_string(),
            });
            seen.insert(name.to_string());
        }
    };
    push_entry(&mut entries, &mut seen, &provider_name, &provider, &model);
    for p in &cfg.providers {
        if p.name == provider_name {
            continue;
        }
        let mdl = if p.model.is_empty() { &model } else { &p.model };
        push_entry(&mut entries, &mut seen, &p.name, p, mdl);
    }
    let pool = byteai_provider::pool::ProviderPool::new(entries);
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(pool, agent_cfg, tools, data_dir.clone())));

    let mut app = App::new(model.clone(), provider.name.clone(), tools_count, count_skills(&data_dir));
    app.notify_rx = Some(notify_rx);
    app.iter_cap = cfg.agent.max_iterations;
    // Share the agent's live status so the UI renders what the agent is doing.
    app.live = Some(agent.lock().await.live.clone());
    // Warm the model list for Ctrl+Tab switching (best-effort, no block).
    if let Ok(ids) = agent.lock().await.pool.client().list_models().await {
        if let Some(pos) = ids.iter().position(|m| m == &model) {
            app.model_idx = pos;
        }
        app.models = ids;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // NOTE: EnableMouseCapture intentionally NOT used — it hijacks the mouse
    // so macOS drag-to-select / ⌘C never reaches the terminal. Without it,
    // you can select and copy any text on screen (macOS Terminal: Option+drag
    // for block selection; iTerm2: ⌥⌘ drag). Mouse clicks for tool cards are
    // handled by the key bindings instead.
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &agent, &mut app).await;

    // Abort any in-flight turn.
    if let Some(h) = app.turn_task.take() {
        h.abort();
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    agent: &Arc<tokio::sync::Mutex<Agent>>,
    app: &mut App,
) -> anyhow::Result<()> {
    // Loaded once: auto-review thresholds are read from config.
    let rcfg = crate::config::load()?;
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.drain();

        // Auto-review heavy turns in the background (non-blocking): self-critique
        // the last exchange and record a durable lesson. Beyond Hermes: it is
        // triggered automatically by config thresholds and never blocks the chat.
        if let Some(outcome) = app.last_outcome.take() {
            let heavy = outcome.tool_calls_made >= rcfg.agent.auto_review_min_tools
                || outcome.iterations >= rcfg.agent.auto_review_min_iters;
            if rcfg.agent.auto_review_enabled && heavy {
                let a = agent.clone();
                let tx = app.review_tx.clone();
                let data_dir = crate::config::data_dir();
                let (tools, iters) = (outcome.tool_calls_made, outcome.iterations);
                tokio::spawn(async move {
                    if let Some(line) = run_auto_review(&a, data_dir, tools, iters).await {
                        let _ = tx.send(line);
                    }
                });
            }
        }

        // Auto-run any queued prompts once the current turn finishes, in
        // the order they were typed (echo=false: already shown in chat).
        if !app.busy && app.turn_task.is_none() && !app.pending_queue.is_empty() {
            let next = app.pending_queue.remove(0);
            spawn_turn(agent, app, &next, false);
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let ev = event::read()?;
        // Mouse: click a tool card to focus/expand it, wheel to scroll.
        if let Event::Mouse(m) = ev {
            handle_mouse(app, m);
            continue;
        }
        if let Event::Key(key) = ev {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // Ctrl+C: interrupt in-flight turn, else quit.
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                if app.busy {
                    interrupt_turn(app, "Ctrl+C");
                    continue;
                }
                break;
            }
            // Interactive picker: Up/Down/Enter/Esc drive the list.
            if app.picker.is_some() {
                match key.code {
                    KeyCode::Esc => {
                        app.picker = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(p) = app.picker.as_mut() {
                            p.sel = p.sel.saturating_sub(1);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(p) = app.picker.as_mut() {
                            let n = p.items.len().max(1);
                            p.sel = (p.sel + 1).min(n - 1);
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(p) = app.picker.take() {
                            // Debounce: identical picker selection within 300ms
                            // (double-Enter / key repeat) applies only once.
                            let sig = format!(
                                "pick:{}",
                                p.values.get(p.sel.min(p.values.len().saturating_sub(1))).cloned().unwrap_or_default()
                            );
                            let fresh = app
                                .last_cmd
                                .as_ref()
                                .map(|(c, t)| !(c == &sig && t.elapsed() < std::time::Duration::from_millis(300)))
                                .unwrap_or(true);
                            if fresh {
                                app.last_cmd = Some((sig, std::time::Instant::now()));
                                run_pick(agent, app, p).await;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            // Ctrl+Shift+K / Ctrl+Shift+J: scroll up/down (jcode).
            if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT) {
                app.follow_bottom = false;
                app.scroll = (app.scroll + 1).min(app.max_scroll.max(1));
                continue;
            }
            if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT) {
                app.scroll = app.scroll.saturating_sub(1);
                if app.scroll == 0 {
                    app.follow_bottom = true;
                }
                continue;
            }
            // Alt+U / Alt+D: page up/down (jcode).
            if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::ALT) {
                app.follow_bottom = false;
                app.scroll = (app.scroll + 20).min(app.max_scroll.max(1));
                continue;
            }
            if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::ALT) {
                app.scroll = app.scroll.saturating_sub(20);
                if app.scroll == 0 {
                    app.follow_bottom = true;
                }
                continue;
            }
            // Ctrl+Tab / Ctrl+Shift+Tab: next/prev model (jcode).
            if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::CONTROL) {
                if !app.models.is_empty() {
                    let delta = if key.modifiers.contains(KeyModifiers::SHIFT) { -1i64 } else { 1i64 };
                    let len = app.models.len() as i64;
                    app.model_idx = (app.model_idx as i64 + delta).rem_euclid(len) as usize;
                    let m = app.models[app.model_idx].clone();
                    agent.lock().await.config.model = m.clone();
                    app.model = m;
                    app.add_meta(format!("  model -> {}", app.model));
                } else {
                    app.add_meta("  no model list (provider offline)");
                }
                continue;
            }
            // Tool-card focus navigation. Once a card is focused, Tab /
            // Shift+Tab cycle through the cards (Enter expands). Shift+Tab
            // also starts focusing even when nothing is focused yet; plain
            // Tab keeps its existing role (command mode on empty input)
            // until there's a focused card to move.
            let plain_tab = key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::CONTROL);
            if plain_tab && (key.modifiers.contains(KeyModifiers::SHIFT) || app.card_focus.is_some()) {
                if !app.card_ids().is_empty() {
                    let dir = if key.modifiers.contains(KeyModifiers::SHIFT) { -1 } else { 1 };
                    app.cycle_card_focus(dir);
                }
                continue;
            }
            // Enter with an empty input and a focused card toggles the card's
            // expanded view (send is a no-op for empty input anyway).
            if key.code == KeyCode::Enter
                && app.input.trim().is_empty()
                && app.card_focus.is_some()
                && app.pending_cmd.is_none() {
                    app.toggle_card();
                    continue;
                }
            // c (with an empty input + a focused card) copies the card's full
            // output to the clipboard — L2 mnemonic, like `c`ommit in lazygit.
            if key.code == KeyCode::Char('c')
                && app.input.is_empty()
                && !app.is_command
                && app.card_focus.is_some() {
                    let id = app.card_focus.unwrap_or(0);
                    let out: String = app
                        .log
                        .iter()
                        .find_map(|e| match e {
                            LogEntry::ToolCard { id: cid, output, .. } if *cid == id => Some(output.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();
                    if out.is_empty() {
                        app.add_error("  card has no output to copy");
                    } else {
                        match clipboard_copy(&out) {
                            Ok(msg) => app.add_meta(format!("  ⧉ {msg}")),
                            Err(e) => app.add_error(&format!("  copy failed: {e}")),
                        }
                    }
                    continue;
                }
            match key.code {
                KeyCode::Esc => {
                    if app.busy {
                        // Esc interrupts the in-flight response (like Ctrl+C).
                        interrupt_turn(app, "Esc");
                    } else {
                        app.input.clear();
                        app.is_command = false;
                        app.palette_idx = 0;
                        app.pending_cmd = None;
                        app.pending_label = None;
                        app.card_focus = None;
                    }
                }
                KeyCode::Enter => {
                    let text = app.input.trim().to_string();
                    // Capture the highlighted palette index BEFORE resetting —
                    // Enter on a palette item must run THAT item, not matches[0].
                    let sel = app.palette_idx;
                    app.input.clear();
                    app.is_command = false;
                    app.palette_idx = 0;
                    if text.is_empty() {
                        continue;
                    }
                    // Hermes-style prompt: if a command is awaiting its argument,
                    // send the typed text as that argument.
                    if let Some(cmd) = app.pending_cmd.take() {
                        app.pending_label = None;
                        let full = format!("{cmd} {text}");
                        handle_command(agent, app, full.trim()).await;
                        continue;
                    }
                    // In command mode, Enter runs the selected palette command
                    // (or completes a unique prefix), like jcode.
                    if let Some(cmd_line) = text.strip_prefix('/') {
                        let mut cmd_line = cmd_line.trim().to_string();
                        let matches = matching_commands(&cmd_line);
                        if matches.is_empty() {
                            // Not a known command name or prefix — run as-is
                            // so /model xyz etc. still work after the name.
                        } else if cmd_line.is_empty() && !matches.is_empty() {
                            // Just "/" -> run the highlighted palette command.
                            cmd_line = matches[sel.min(matches.len() - 1)].to_string();
                        } else if matches.len() == 1 && matches[0] != cmd_line {
                            // Unique prefix -> complete to the full command.
                            cmd_line = matches[0].to_string();
                        } else if matches.len() > 1 && !matches.contains(&cmd_line.as_str()) {
                            // Ambiguous prefix -> run the highlighted match.
                            cmd_line = matches[sel.min(matches.len() - 1)].to_string();
                        }
                        // Debounce: identical command within 300ms (double-Enter
                        // or key repeat) runs only once.
                        let fresh = app
                            .last_cmd
                            .as_ref()
                            .map(|(c, t)| !(c == &cmd_line && t.elapsed() < std::time::Duration::from_millis(300)))
                            .unwrap_or(true);
                        if fresh {
                            app.last_cmd = Some((cmd_line.clone(), std::time::Instant::now()));
                            handle_command(agent, app, &cmd_line).await;
                        }
                        continue;
                    }
                    spawn_turn(agent, app, &text, true);
                }
                KeyCode::Up => {
                    if app.is_command {
                        // Move selection UP the palette (index 0 = top).
                        app.palette_idx = app.palette_idx.saturating_sub(1);
                    } else {
                        app.follow_bottom = false;
                        app.scroll = (app.scroll + 1).min(app.max_scroll.max(1));
                    }
                }
                KeyCode::Down => {
                    if app.is_command {
                        let n = matching_commands(&app.input[1..]).len();
                        if n > 0 {
                            app.palette_idx = (app.palette_idx + 1).min(n - 1);
                        }
                    } else {
                        app.scroll = app.scroll.saturating_sub(1);
                        if app.scroll == 0 {
                            app.follow_bottom = true;
                        }
                    }
                }
                KeyCode::PageUp => {
                    app.follow_bottom = false;
                    app.scroll = (app.scroll + 20).min(app.max_scroll.max(1));
                }
                KeyCode::PageDown => {
                    app.scroll = app.scroll.saturating_sub(20);
                    if app.scroll == 0 {
                        app.follow_bottom = true;
                    }
                }
                KeyCode::Tab => {
                    if app.is_command {
                        // Complete to the selected (or first) matching command.
                        let matches = matching_commands(&app.input[1..]);
                        if !matches.is_empty() {
                            let pick = matches[app.palette_idx.min(matches.len() - 1)];
                            app.input = format!("/{pick} ");
                        }
                    } else if app.input.is_empty() {
                        // Enter command mode with exactly ONE leading slash.
                        // (Tab with text already typed is a no-op so "hello"
                        // can never become the command "/hello".)
                        app.is_command = true;
                        app.input.push('/');
                    }
                }
                KeyCode::Char(c) => {
                    // Only ONE leading slash: typing '/' when the input is
                    // exactly "/" is ignored so "/" never becomes "//".
                    // Slashes later in the line (URLs, /path args) pass
                    // through untouched.
                    if c == '/' && app.input == "/" {
                        continue;
                    }
                    app.input.push(c);
                    app.is_command = app.input.starts_with('/');
                    if app.is_command {
                        app.palette_idx = 0;
                    }
                }
                KeyCode::Backspace => {
                    app.input.pop();
                    app.is_command = app.input.starts_with('/');
                    app.palette_idx = 0;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Map a mouse event onto the transcript:
/// - left-click on a tool card → focus it; clicking the focused card again
///   expands/collapses it
/// - scroll wheel → scroll the transcript (3 rows per notch)
///
/// The row→card mapping is recomputed on demand with the same line builder
/// as draw_chat, so clicks land on the right card even after wrapping.
fn handle_mouse(app: &mut App, m: crossterm::event::MouseEvent) {
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let chat_y = app.last_chat_y as usize;
            let chat_h = app.last_chat_h as usize;
            if chat_h == 0 || app.last_chat_w == 0 {
                return;
            }
            let content_row = (m.row as usize).saturating_sub(chat_y);
            if content_row >= chat_h {
                return; // clicked below the chat box (input/status rows)
            }

            // Rebuild lines + per-row card map with the exact same wrapping
            // ratatui uses (only on click — cheap, never per-frame).
            let (lines, card_map) = build_chat_lines(app);
            let mut row_map: Vec<Option<u64>> = Vec::new();
            for (line, card) in lines.iter().zip(card_map.iter()) {
                let rows = Paragraph::new(line.clone())
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .line_count(app.last_chat_w)
                    .max(1);
                for _ in 0..rows {
                    row_map.push(*card);
                }
            }
            let offset = chat_offset(row_map.len(), chat_h, app.follow_bottom, app.scroll);
            let idx = offset + content_row;
            if let Some(Some(id)) = row_map.get(idx) {
                let id = *id;
                if app.card_focus == Some(id) {
                    app.toggle_card(); // second click expands/collapses
                } else {
                    app.card_focus = Some(id);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            app.follow_bottom = false;
            app.scroll = (app.scroll + 3).min(app.max_scroll.max(1));
        }
        MouseEventKind::ScrollDown => {
            app.scroll = app.scroll.saturating_sub(3);
            if app.scroll == 0 {
                app.follow_bottom = true;
            }
        }
        _ => {}
    }
}

/// Background self-review after a heavy turn: critique the last assistant
/// exchange, record a durable lesson (data_dir/reviews.log), and return a
/// one-line summary to surface in the transcript. Runs on its own task so it
/// never blocks the chat. Returns None if there is nothing to review.
async fn run_auto_review(
    agent: &Arc<tokio::sync::Mutex<Agent>>,
    data_dir: std::path::PathBuf,
    tools: u32,
    iters: u32,
) -> Option<String> {
    let (last_asst, model) = {
        let g = agent.lock().await;
        if g.history.is_empty() {
            return None;
        }
        let last = g
            .history
            .iter()
            .rev()
            .find_map(|m| match &m.role {
                byteai_types::Role::Assistant => m.content.clone(),
                _ => None,
            })
            .unwrap_or_default();
        if last.trim().is_empty() {
            return None;
        }
        (last, g.config.model.clone())
    };

    let prompt = format!(
        "You are reviewing the last exchange for correctness and completeness.\n\
         Be concise (2-3 sentences). First give a verdict (OK / needs-work), then one \
         concrete, reusable lesson.\n\n\
         LAST ASSISTANT OUTPUT:\n{last_asst}"
    );
    let msg = byteai_types::Message::user(&prompt);
    let review = agent
        .lock()
        .await
        .pool
        .client()
        .chat(&model, &[msg], &[], Some(512))
        .await
        .map(|(text, _, _)| text)
        .unwrap_or_default();
    let review = review.trim().to_string();
    if review.is_empty() {
        return None;
    }

    // Durable lesson record (append-only).
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(_dir) = std::fs::create_dir_all(data_dir.join("reviews")) {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data_dir.join("reviews").join("reviews.log"))
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{ts}] tools={tools} iters={iters}\n{review}\n")
            });
    }

    let first_line = review.lines().next().unwrap_or(&review).to_string();
    Some(format!("auto-review ({tools} tools, {iters} iters): {first_line}"))
}

/// Abort the in-flight turn (if any) and return the UI to idle. Used by
/// Ctrl+C and Esc — both interrupt a streaming response. Queued prompts are
/// preserved: the user typed them deliberately, and interrupt only stops the
/// CURRENT turn, not their follow-ups.
fn interrupt_turn(app: &mut App, how: &str) {
    if let Some(h) = app.turn_task.take() {
        h.abort();
    }
    app.busy = false;
    app.busy_since = None;
    app.turn_rx = None;
    app.add_meta(format!("  ⚡ interrupted ({how})"));
}

/// Execute the selected picker item — the command's final, user-friendly
/// purpose (set default model, switch provider, resume session, inspect a
/// gate ledger).
async fn run_pick(agent: &Arc<tokio::sync::Mutex<Agent>>, app: &mut App, pick: Picker) {
    let idx = pick.sel.min(pick.values.len().saturating_sub(1));
    match pick.action {
        PickAction::SetModel { provider } => {
            let m = pick.values[idx].clone();
            let mut cfg = crate::config::load().unwrap_or_default();
            if let Err(e) = crate::config::set_model(&mut cfg, &provider, &m) {
                app.add_error(&format!("  could not persist model: {e:#}"));
            }
            agent.lock().await.config.model = m.clone();
            app.model = m.clone();
            // Keep the Ctrl+Tab cycle in sync with the new default.
            if let Some(pos) = app.models.iter().position(|x| x == &m) {
                app.model_idx = pos;
            }
            app.add_meta(format!("  model -> {m} (saved as default)"));
        }
        PickAction::SwitchProvider => {
            let name = pick.values[idx].clone();
            let cfg = crate::config::load().unwrap_or_default();
            let provider = crate::config::resolve_provider(&cfg, Some(&name), None, None);
            if provider.name != name {
                app.add_error(&format!("  provider '{name}' not found"));
                return;
            }
            match byteai_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
                Ok(client) => {
                    let mut cfg2 = crate::config::load().unwrap_or_default();
                    let _ = crate::config::set_default_provider(&mut cfg2, &name);
                    let model = crate::config::resolve_model(&cfg, None, &provider);
                    {
                        let mut g = agent.lock().await;
                        g.pool.replace_active(&name, client, &model);
                        g.config.model = model.clone();
                    }
                    app.provider = name.clone();
                    app.model = model.clone();
                    if let Ok(ids) = agent.lock().await.pool.client().list_models().await {
                        app.models = ids;
                        if let Some(pos) = app.models.iter().position(|x| x == &model) {
                            app.model_idx = pos;
                        }
                    }
                    app.add_meta(format!("  provider -> {name} · model -> {model} (saved as default)"));
                }
                Err(e) => app.add_error(&format!("  provider {name}: {e:#}")),
            }
        }
        PickAction::ResumeSession => {
            let id = pick.values[idx].clone();
            resume_session(agent, app, &id).await;
        }
        PickAction::GatesStatus => {
            let path = pick.values[idx].clone();
            let g = agent.lock().await;
            let tool = g.tools.get("gates");
            drop(g);
            match tool {
                Some(tool) => {
                    let args = serde_json::json!({"action": "status", "path": path});
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("gates tool not available"),
            }
        }
    }
}

/// Find gate ledgers (GATES.md at cwd root or under .unlazy/<scope>/gates/)
/// for the /gates picker.
fn find_gate_files(root: &str) -> Vec<String> {
    let mut found = Vec::new();
    let root = std::path::Path::new(root);
    let root_ledger = root.join("GATES.md");
    if root_ledger.is_file() {
        found.push(root_ledger.display().to_string());
    }
    let unlazy = root.join(".unlazy");
    if unlazy.is_dir()
        && let Ok(entries) = std::fs::read_dir(&unlazy) {
            for e in entries.flatten() {
                let p = e.path();
                let gates_dir = p.join("gates");
                if gates_dir.is_dir()
                    && let Ok(gates) = std::fs::read_dir(&gates_dir) {
                        for g in gates.flatten() {
                            let gp = g.path();
                            if gp.is_file() && gp.extension().map(|x| x == "md").unwrap_or(false) {
                                found.push(gp.display().to_string());
                            }
                        }
                    }
                let plan = p.join("GATES.md");
                if plan.is_file() {
                    found.push(plan.display().to_string());
                }
            }
        }
    found
}

/// Rebuild the visible transcript from a message history (user + assistant
/// lines only; tool internals are not replayed as cards). Used by
/// /retry, /undo, /compress, /resume and the session picker.
fn rebuild_log_from_history(app: &mut App, history: &[byteai_types::Message]) {
    app.log.clear();
    for m in history {
        match m.role {
            byteai_types::Role::User => {
                app.add_user(m.content.as_deref().unwrap_or(""));
            }
            _ => {
                if let Some(c) = &m.content {
                    app.add_assistant(c);
                }
            }
        }
    }
    app.scroll = 0;
    app.follow_bottom = true;
}

/// Resume a named session: replace agent history, rebuild the transcript,
/// restore the model. Shared by the /session picker and /resume <name>.
async fn resume_session(agent: &Arc<tokio::sync::Mutex<Agent>>, app: &mut App, id: &str) {
    match crate::session::load(id) {
        Ok(sf) => {
            let msgs = sf.messages.clone();
            let model = sf.model.clone();
            {
                let mut g = agent.lock().await;
                g.history = msgs.clone();
                g.config.model = model.clone();
            }
            app.model = model;
            rebuild_log_from_history(app, &msgs);
            app.add_meta(format!("  resumed session {id} ({} msgs)", msgs.len()));
        }
        Err(e) => app.add_error(&format!("  could not load session: {e:#}")),
    }
}

/// Copy text to the system clipboard (macOS pbcopy, Linux xclip/wl-copy).
/// Returns a short confirmation or an error string.
fn clipboard_copy(text: &str) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    let cmd = ("pbcopy", Vec::<String>::new());
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = {
        let mut args = Vec::new();
        let bin = if std::path::Path::new("/usr/bin/xclip").exists() || std::path::Path::new("/usr/bin/xclip").exists() {
            args.push("-selection".to_string());
            args.push("clipboard".to_string());
            "xclip"
        } else if std::path::Path::new("/usr/bin/wl-copy").exists() || std::path::Path::new("/usr/bin/wl-copy").exists() {
            "wl-copy"
        } else {
            args.push("-selection".to_string());
            args.push("clipboard".to_string());
            "xclip"
        };
        (bin, args)
    };
    #[cfg(not(unix))]
    let cmd = ("", Vec::<String>::new());

    if cmd.0.is_empty() {
        return Err("no clipboard tool (needs pbcopy / xclip / wl-copy)".into());
    }
    let mut c = std::process::Command::new(cmd.0);
    c.args(&cmd.1);
    c.stdin(std::process::Stdio::piped());
    c.stdout(std::process::Stdio::null());
    c.stderr(std::process::Stdio::null());
    let mut child = c.spawn().map_err(|e| format!("{e:#}"))?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        stdin.write_all(text.as_bytes()).map_err(|e| format!("{e:#}"))?;
        stdin.flush().ok();
    }
    child.wait().map_err(|e| format!("{e:#}"))?;
    Ok(format!("{} chars copied", text.chars().count()))
}

/// Git working-tree summary for /diff: `git status --short` + `git diff --stat`.
/// Fast and read-only. Returns lines or an error.
fn git_diff_summary() -> Result<Vec<String>, String> {
    let run = |args: &[&str]| -> Result<String, String> {
        let out = std::process::Command::new("git")
            .args(args)
            .output()
            .map_err(|e| format!("git: {e:#}"))?;
        if !out.status.success() {
            return Err(format!("git {}: non-zero exit", args.join(" ")));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };
    let mut lines = Vec::new();
    let status = run(&["status", "--short"]).map_err(|_| "not a git repository (or git missing)".to_string())?;
    for l in status.lines().take(30) {
        lines.push(l.to_string());
    }
    if lines.is_empty() {
        lines.push("(clean working tree)".to_string());
    }
    let stat = run(&["diff", "--stat"]);
    if let Ok(s) = stat {
        for l in s.lines() {
            lines.push(l.to_string());
        }
    }
    Ok(lines)
}


/// Open the session-resume picker (shared by /session and /resume).
fn open_session_picker(app: &mut App) {
    match crate::session::list() {
        Ok(list) if list.is_empty() => app.add_meta("  no saved sessions — type /save to save"),
        Ok(list) => {
            let items: Vec<String> = list.iter().map(|s| {
                let msgs = s.messages.len();
                let last = s.messages.last().and_then(|m| m.content.clone()).unwrap_or_default();
                let preview: String = last.chars().take(40).collect();
                format!("{} · {msgs} msgs · {preview}", s.id)
            }).collect();
            let values: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
            app.picker = Some(Picker {
                title: "sessions — scroll, Enter resumes".into(),
                items, values, sel: 0,
                action: PickAction::ResumeSession,
            });
        }
        Err(e) => app.add_error(&format!("session list failed: {e:#}")),
    }
}

/// Spawn the agent turn in a background task; stream events via a channel
/// so the UI stays responsive with a spinner + live token rendering.
fn spawn_turn(agent: &Arc<tokio::sync::Mutex<Agent>>, app: &mut App, text: &str, echo: bool) {
    // Always echo the question into the chat immediately so the user sees
    // it right away — even if the previous turn is still running.
    if echo {
        app.add_user(text);
    }
    if app.busy {
        app.pending_queue.push(text.to_string());
        app.add_meta("  ⏳ queued (will run when the current answer finishes)");
        return;
    }
    let prompt = text.to_string();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<TurnMsg>();
    app.turn_rx = Some(rx);

    let a2 = Arc::clone(agent);
    let handle = tokio::spawn(async move {
        // Acquire the agent lock defensively: if an interrupted turn is still
        // unwinding (abort is async — the old task may not have dropped its
        // MutexGuard yet), waiting forever would freeze the UI. Give it a
        // bounded window and surface an error instead of hanging.
        let mut guard = match tokio::time::timeout(Duration::from_secs(15), a2.lock()).await {
            Ok(g) => g,
            Err(_) => {
                let _ = tx.send(TurnMsg::Err(
                    "previous turn is still shutting down — try again in a moment".into(),
                ));
                return;
            }
        };
        let tx_text = tx.clone();
        let mut sink = move |t: &str| {
            let _ = tx_text.send(TurnMsg::Text(t.to_string()));
        };
        let tx_tool = tx.clone();
        let mut toolsink = move |o: &byteai_types::ToolOutcome| {
            let _ = tx_tool.send(TurnMsg::Tool(o.clone()));
        };
        let fut = guard.run(&prompt, &mut sink, &mut toolsink);
        match tokio::time::timeout(Duration::from_secs(600), fut).await {
            Ok(Ok(outcome)) => {
                let _ = tx.send(TurnMsg::Done(outcome));
            }
            Ok(Err(e)) => {
                let _ = tx.send(TurnMsg::Err(format!("{e:#}")));
            }
            Err(_) => {
                let _ = tx.send(TurnMsg::Err("turn timed out (600s)".into()));
            }
        }
    });
    app.turn_task = Some(handle);
    app.busy = true;
    app.busy_since = Some(Instant::now());
    // Reset the end-of-turn ribbon accumulator for the new turn.
    app.turn_tools.clear();
    app.turn_start = Some(Instant::now());
    // Clear stale error note from the live status for the new turn.
    if let Some(ref l) = app.live
        && let Ok(mut ls) = l.lock() {
            ls.note.clear();
            ls.phase = byteai_core::Phase::Understanding;
            ls.active_tools.clear();
        }
}

/// Run the `dan_methodology` tool and render its output into the chat log.
/// Used by `/dan` (and aliases) so all entry points share one code path.
async fn run_dan_tool(app: &mut App, agent: &Arc<tokio::sync::Mutex<Agent>>, args: serde_json::Value) {
    let g = agent.lock().await;
    let tool = g.tools.get("dan_methodology");
    drop(g);
    match tool {
        Some(tool) => {
            let outcome = tool.execute(args).await;
            app.add_meta(format!("  [dan methodology · {} ms]", outcome.elapsed_ms));
            for line in outcome.output.lines() {
                app.add_meta(format!("  {line}"));
            }
        }
        None => app.add_error("dan_methodology tool not available"),
    }
}

/// All dan methodology topics (kept in sync with the dan_methodology tool).
const DAN_TOPICS: &[&str] = &[
    "overview", "harness_engineering", "skills_system",
    "agent_sandboxes", "multi_agent", "model_selection",
    "security", "observability", "context_engineering",
    "planning", "local_models", "agent_threads",
    "software_factory", "prompt_engineering", "agents_learning",
];

/// Fuzzy topic matching for `/dan <topic>`: exact match first, then a unique
/// prefix or substring match (spaces normalized to underscores), so
/// `/dan multi`, `/dan sandbox`, `/dan context eng` all resolve.
fn match_dan_topic(norm: &str) -> Option<&'static str> {
    let norm = norm.trim().replace(' ', "_").to_lowercase();
    if norm.is_empty() {
        return None;
    }
    if let Some(exact) = DAN_TOPICS.iter().find(|t| **t == norm) {
        return Some(exact);
    }
    let hits: Vec<&str> = DAN_TOPICS
        .iter()
        .filter(|t| t.starts_with(&norm) || t.contains(&norm))
        .copied()
        .collect();
    if hits.len() == 1 {
        Some(hits[0])
    } else {
        None
    }
}

async fn handle_command(agent: &Arc<tokio::sync::Mutex<Agent>>, app: &mut App, cmd_line: &str) {
    let parts: Vec<&str> = cmd_line.split_whitespace().collect();
    let cmd = parts.first().unwrap_or(&"");
    match *cmd {
        "help" | "h" | "?" | "commands" => {
            app.add_assistant("┌─ General");
            app.add_assistant("/help           — this message");
            app.add_assistant("/status         — session info (model, tokens, context)");
            app.add_assistant("/version        — show version");
            app.add_assistant("/keys           — show keybindings");
            app.add_assistant("/tools          — list available tools");
            app.add_assistant("/config         — show config path");
            app.add_assistant("/usage [reset]  — token usage, or zero the counters");
            app.add_assistant("/clear          — clear conversation");
            app.add_assistant("┌─ Session");
            app.add_assistant("/new            — start a fresh session");
            app.add_assistant("/retry          — resend the last message");
            app.add_assistant("/undo [N]       — back up N turns and re-run");
            app.add_assistant("/compress       — summarize + compact the context");
            app.add_assistant("/title [name]   — name/save the session");
            app.add_assistant("/save [name]    — save session to disk");
            app.add_assistant("/session        — scroll-pick a saved session to resume");
            app.add_assistant("/resume <id>    — resume a session by name");
            app.add_assistant("┌─ Model & Provider");
            app.add_assistant("/model <name>   — set + persist model (no arg: scroll-pick)");
            app.add_assistant("/provider       — scroll-pick a provider as default");
            app.add_assistant("/addprovider    — add provider+model (e.g. /addprovider name url model)");
            app.add_assistant("/reload         — reload config.toml into the session");
            app.add_assistant("┌─ Agent Tools");
            app.add_assistant("/route          — route a task to the best model (asks for it)");
            app.add_assistant("/council        — multi-model deliberation vote (asks for it)");
            app.add_assistant("/govern         — constitutional guardrail check (asks for it)");
            app.add_assistant("/gates          — acceptance ledger (asks for the ledger)");
            app.add_assistant("/subagent       — spawn parallel subagents (asks for the goal)");
            app.add_assistant("/swarm          — spawn 3-way swarm (asks for the goal)");
            app.add_assistant("┌─ Intelligence");
            app.add_assistant("/ideas [focus]  — discover top product ideas from real internet problems");
            app.add_assistant("                 (/ideas research <idea> · /ideas build <idea> · /ideas status)");
            app.add_assistant("/github        — discover+score the best skills/tools/harnesses/mcp");
            app.add_assistant("                 (/github skills <cap> · /github harnesses <cap> · /github improve");
            app.add_assistant("                  /github current · /github evaluate <owner/repo> · /github memory · /github graph)");
            app.add_assistant("/github connect [repo] [public|private] — publish this project to GitHub");
            app.add_assistant("/github status — auth+repo status · /github push — push latest");
            app.add_assistant("┌─ Other");
            app.add_assistant("/copy           — copy last response to clipboard");
            app.add_assistant("/diff           — git working-tree summary");
            app.add_assistant("/quit           — exit");
            app.add_assistant("");
            app.add_assistant("  pickers: ↑↓ scroll · Enter select · Esc cancel");
        }
        "model" | "models" => {
            if *cmd == "model"
                && let Some(m) = parts.get(1) {
                    let m = m.to_string();
                    // Persist so the choice survives restart (agent + provider).
                    let mut cfg = crate::config::load().unwrap_or_default();
                    if let Err(e) = crate::config::set_model(&mut cfg, &app.provider, &m) {
                        app.add_error(&format!("  could not persist model: {e:#}"));
                    }
                    agent.lock().await.config.model = m.clone();
                    app.model = m.clone();
                    if let Some(pos) = app.models.iter().position(|x| x == &m) {
                        app.model_idx = pos;
                    }
                    app.add_meta(format!("  model -> {m} (saved as default)"));
                    return;
                }
            // No argument: interactive picker — scroll, Enter sets + persists.
            match agent.lock().await.pool.client().list_models().await {
                Ok(ids) if !ids.is_empty() => {
                    app.models = ids.clone();
                    let sel = ids.iter().position(|x| x == &app.model).unwrap_or(0);
                    app.picker = Some(Picker {
                        title: format!("models on {} — scroll, Enter sets default", app.provider),
                        items: ids.clone(),
                        values: ids,
                        sel,
                        action: PickAction::SetModel { provider: app.provider.clone() },
                    });
                }
                Ok(_) => app.add_meta("  no models reported by provider"),
                Err(_) => app.add_meta("  (provider model list offline)"),
            }
        }
        "provider" => {
            let cfg = crate::config::load().unwrap_or_default();
            match parts.get(1).copied() {
                None => {
                    // Interactive picker: scroll, Enter switches + persists.
                    let items: Vec<String> = cfg.providers.iter().map(|p| {
                        let cur = if p.name == app.provider { "▸ " } else { "  " };
                        let key = if p.resolved_key().is_empty() { " (no key)" } else { "" };
                        format!("{cur}{}{key}", p.name)
                    }).collect();
                    let values: Vec<String> = cfg.providers.iter().map(|p| p.name.clone()).collect();
                    let sel = values.iter().position(|x| x == &app.provider).unwrap_or(0);
                    app.picker = Some(Picker {
                        title: "providers — scroll, Enter switches default".into(),
                        items, values, sel,
                        action: PickAction::SwitchProvider,
                    });
                }
                Some(name) => {
                    // Switch provider at runtime: rebuild the client and model.
                    let provider = crate::config::resolve_provider(&cfg, Some(name), None, None);
                    if provider.name != name {
                        app.add_error(&format!("  provider '{name}' not found — see /provider"));
                        return;
                    }
                    match byteai_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
                        Ok(client) => {
                            // Persist as default provider.
                            let mut cfg2 = crate::config::load().unwrap_or_default();
                            let _ = crate::config::set_default_provider(&mut cfg2, name);
                            let model = crate::config::resolve_model(&cfg, None, &provider);
                            {
                                let mut g = agent.lock().await;
                                g.pool.replace_active(name, client, &model);
                                g.config.model = model.clone();
                            }
                            app.provider = name.to_string();
                            app.model = model.clone();
                            // Refresh the model list for the new provider.
                            if let Ok(ids) = agent.lock().await.pool.client().list_models().await {
                                app.models = ids;
                            }
                            app.add_meta(format!("  provider -> {name}, model -> {model}"));
                        }
                        Err(e) => app.add_error(&format!("  provider {name}: {e:#}")),
                    }
                }
            }
        }
        "addprovider" | "provider-add" => {
            let name = parts.get(1).copied().unwrap_or("");
            let url = parts.get(2).copied().unwrap_or("");
            let model = parts.get(3).copied().unwrap_or("");
            let key = parts.get(4).copied().unwrap_or("");
            if name.is_empty() || url.is_empty() {
                app.add_error("usage: /addprovider <name> <base_url> <model> [api_key]");
                app.add_error("       key can be a literal or an env var name prefixed with env: (e.g. env:MY_KEY)");
                return;
            }
            let (key_val, env_val) = if let Some(env) = key.strip_prefix("env:") {
                ("".to_string(), env.to_string())
            } else {
                (key.to_string(), String::new())
            };
            let mut cfg = crate::config::load().unwrap_or_default();
            match crate::config::add_provider(&mut cfg, name, url, &key_val, &env_val, model) {
                Ok(()) => {
                    app.add_meta(format!("  added provider '{name}' ({url}, model {model})"));
                    // Switch to it immediately.
                    match byteai_provider::Client::new(url.to_string(), key_val) {
                        Ok(client) => {
                            let m = if model.is_empty() { app.model.clone() } else { model.to_string() };
                            {
                                let mut g = agent.lock().await;
                                g.pool.replace_active(name, client, &m);
                                g.config.model = m.clone();
                            }
                            app.provider = name.to_string();
                            app.model = m.clone();
                            app.add_meta(format!("  now on provider -> {name}, model -> {m}"));
                        }
                        Err(e) => app.add_error(&format!("  connected but client failed: {e:#}")),
                    }
                }
                Err(e) => app.add_error(&format!("  {e:#}")),
            }
        }
        "tools" => {
            let g = agent.lock().await;
            let names = g.tools.names();
            let mode = if g.config.tool_select { "auto" } else { "all" };
            let selected = if g.config.tool_select {
                let defs = byteai_core::select_tools(
                    &g.tools.defs(),
                    "tools",
                    byteai_core::ToolSelectStrategy::Auto,
                    g.config.tool_select_max,
                );
                defs.len()
            } else {
                names.len()
            };
            let toggle = match parts.get(1).copied() {
                Some("all") | Some("full") => Some(false),
                Some("auto") | Some("select") | Some("smart") => Some(true),
                _ => None,
            };
            // /tools for <task> — preview which tools WOULD be selected for
            // an arbitrary task, without running it. This is how you "talk"
            // to the selector: ask it to show its work for any request.
            let preview = match parts.get(1).copied() {
                Some("for") | Some("preview") => {
                    let task = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                    Some(task)
                }
                _ => None,
            };
            drop(g);
            if let Some(on) = toggle {
                let mut g = agent.lock().await;
                g.config.tool_select = on;
                drop(g);
                app.add_meta(format!(
                    "  smart tool selection: {} (next turn)",
                    if on { "AUTO" } else { "ALL tools" }
                ));
            } else if let Some(task) = preview {
                if task.trim().is_empty() {
                    app.add_error("usage: /tools for <task> — e.g. /tools for create a github pr");
                } else {
                    let g = agent.lock().await;
                    let defs = byteai_core::select_tools(
                        &g.tools.defs(),
                        &task,
                        byteai_core::ToolSelectStrategy::Auto,
                        g.config.tool_select_max,
                    );
                    drop(g);
                    app.add_meta(format!(
                        "  [tool selection for {:?} · {}/{}]",
                        task.trim(),
                        defs.len(),
                        names.len()
                    ));
                    app.add_meta(format!("  {}", defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")));
                }
            } else {
                app.add_meta(format!(
                    "  tools ({selected}/{}) mode={mode} — /tools auto | /tools all | /tools for <task>",
                    names.len()
                ));
            }
        }
        "dan" | "methodology" | "d" => {
            // /dan                   → full methodology overview + topic index
            // /dan <topic>           → deep section (fuzzy: "multi", "sandbox",
            //                          "context eng" all work)
            // /dan query <text>      → free-text lookup across all topics
            // /dan apply <anything>  → run <anything> as a task, dan-primed
            // /dan help              → topic list
            let rest = parts.get(1).copied().unwrap_or("");
            match rest {
                "help" | "?" | "-h" => {
                    app.add_meta("  dan methodology topics: overview, harness_engineering, skills_system, agent_sandboxes, multi_agent, model_selection, security, observability, context_engineering, planning, local_models, agent_threads, software_factory, prompt_engineering, agents_learning");
                    app.add_meta("  usage: /dan <topic> | /dan query <text> | /dan apply <task>");
                }
                "apply" | "run" => {
                    let task = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                    if task.is_empty() {
                        app.add_error("usage: /dan apply <task>");
                    } else {
                        // The dan brief is already in the system prompt; prime
                        // the turn explicitly so the agent consults the
                        // methodology before acting.
                        spawn_turn(agent, app, &format!("Apply the Dan methodology (plan first, use harness tools, sandbox + review, capture a skill after): {task}"), true);
                    }
                }
                "query" => {
                    let q = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                    run_dan_tool(app, agent, serde_json::json!({ "query": q })).await;
                }
                "" => {
                    run_dan_tool(app, agent, serde_json::json!({})).await;
                }
                _ => {
                    // Fuzzy topic match (spaces, prefixes, partials all work);
                    // fall back to a free-text query if nothing unique matches.
                    match match_dan_topic(rest) {
                        Some(topic) => run_dan_tool(app, agent, serde_json::json!({ "topic": topic })).await,
                        None => {
                            app.add_meta(format!("  [dan: no exact topic for {rest:?} — searching all sections]"));
                            run_dan_tool(app, agent, serde_json::json!({ "query": rest })).await;
                        }
                    }
                }
            }
        }
        "gates" => {
            let action = parts.get(1).copied().unwrap_or("status").to_string();
            let path = parts.get(2).map(|s| s.to_string());
            match path {
                Some(path) => {
                    let g = agent.lock().await;
                    let tool = g.tools.get("gates");
                    drop(g);
                    match tool {
                        Some(tool) => {
                            let args = serde_json::json!({"action": action, "path": path});
                            let outcome = tool.execute(args).await;
                            app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                        }
                        None => app.add_error("gates tool not available"),
                    }
                }
                None => {
                    // No path: pick a gate ledger found in the workspace.
                    let found = find_gate_files(".");
                    if found.is_empty() {
                        app.add_meta("  no GATES.md found in cwd — use: /gates status <path>");
                    } else {
                        app.picker = Some(Picker {
                            title: format!("gate ledgers — scroll, Enter to {action}"),
                            items: found.clone(),
                            values: found,
                            sel: 0,
                            action: PickAction::GatesStatus,
                        });
                    }
                }
            }
        }
        "clear" => {
            app.clear();
        }
        "save" | "title" => {
            // Hermes /title: name the current session. Name defaults to a
            // digest of the last user message.
            let name = if let Some(n) = parts.get(1) {
                n.to_string()
            } else {
                let g = agent.lock().await;
                let last_user = g
                    .history
                    .iter()
                    .rev()
                    .find(|m| m.role == byteai_types::Role::User)
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                drop(g);
                let slug: String = last_user
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .take(24)
                    .collect();
                if slug.is_empty() {
                    format!("tui-{}", crate::session::new_id().get(..8).unwrap_or("sess"))
                } else {
                    slug
                }
            };
            let g = agent.lock().await;
            let mut sf = crate::session::from_agent(&g.config.model, &app.provider, g.history.clone(), g.usage.clone());
            sf.id = name.clone();
            drop(g);
            match crate::session::save(&sf) {
                Ok(p) => app.add_meta(format!("  session saved: {} ({name})", p.display())),
                Err(e) => app.add_error(&format!("save failed: {e:#}")),
            }
        }
        "session" | "sessions" => {
            open_session_picker(app);
        }
        "resume" => {
            match parts.get(1).copied() {
                Some(id) => resume_session(agent, app, id).await,
                None => open_session_picker(app),
            }
        }
        "new" => {
            // Hermes /new: fresh session.
            let msgs = agent.lock().await.history.len();
            app.clear();
            app.add_meta(format!("  new session ({} messages discarded)", msgs));
        }
        "retry" | "undo" => {
            // Hermes /retry: resend last user message.
            // Hermes /undo [N]: back up N user turns and re-prompt.
            let n = if *cmd == "undo" {
                parts.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).max(1)
            } else {
                1
            };
            let history = agent.lock().await.history.clone();
            let user_idxs: Vec<usize> = history
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == byteai_types::Role::User)
                .map(|(i, _)| i)
                .collect();
            let Some(&target) = user_idxs.iter().rev().nth(n - 1) else {
                app.add_error(&format!("  only {} user turn(s) to redo", user_idxs.len()));
                return;
            };
            let text = history[target].content.clone().unwrap_or_default();
            if text.is_empty() {
                app.add_error("  last user message is empty");
                return;
            }
            // Truncate history before the target user turn, rebuild the log,
            // then re-run the turn.
            let trimmed: Vec<byteai_types::Message> = history[..target].to_vec();
            {
                let mut g = agent.lock().await;
                g.history = trimmed;
            }
            rebuild_log_from_history(app, &agent.lock().await.history);
            app.add_meta(if *cmd == "retry" {
                "  ↻ retrying last message…".to_string()
            } else {
                format!("  ↻ undid {n} turn(s), re-running…")
            });
            spawn_turn(agent, app, &text, true);
        }
        "compress" => {
            // Hermes /compress: summarize the older context and keep the
            // recent turns, so the window fits the budget again.
            let (history, model) = {
                let g = agent.lock().await;
                (g.history.clone(), g.config.model.clone())
            };
            let keep = 8usize; // messages kept verbatim (besides system)
            if history.len() <= keep + 1 {
                app.add_meta(format!("  context small ({} msgs) — nothing to compress", history.len()));
                return;
            }
            let (system, rest) = match history.split_first() {
                Some((s, r)) if s.role == byteai_types::Role::System => (Some(s.clone()), r.to_vec()),
                _ => (None, history.clone()),
            };
            let (drop, keep_msgs) = if rest.len() > keep {
                rest.split_at(rest.len() - keep)
            } else {
                (&[][..], rest.as_slice())
            };
            // Summarize the dropped messages (best effort; fall back to a
            // plain trim if the provider is offline).
            let dropped_text: String = drop
                .iter()
                .filter_map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let summary = {
                let g = agent.lock().await;
                let prompt = format!(
                    "Summarize the following conversation history in 2-3 concise sentences, keeping all decisions, requirements and file paths. This replaces the full history.\n\n{dropped_text}"
                );
                let msg = byteai_types::Message::user(&prompt);
                g.pool
                    .client()
                    .chat(&model, &[msg], &[], Some(300))
                    .await
                    .map(|(t, _, _)| t.trim().to_string())
            }
            .unwrap_or_else(|_| format!("(previous {} messages trimmed; provider unavailable)", drop.len()));
            let summary = if summary.is_empty() {
                format!("(previous {} messages trimmed)", drop.len())
            } else {
                summary
            };
            let mut new_history = Vec::new();
            if let Some(s) = system {
                new_history.push(s);
            }
            new_history.push(byteai_types::Message::system(format!("Summary of earlier conversation: {summary}")));
            new_history.extend(keep_msgs.iter().cloned());
            {
                let mut g = agent.lock().await;
                g.history = new_history.clone();
            }
            rebuild_log_from_history(app, &new_history);
            app.add_meta(format!(
                "  compressed {} messages → summary ({:.0}% smaller)",
                drop.len(),
                100.0 * drop.len() as f64 / history.len().max(1) as f64
            ));
        }
        "status" => {
            let g = agent.lock().await;
            let msgs = g.history.len();
            let chars: usize = g.history.iter().filter_map(|m| m.content.as_ref()).map(|c| c.chars().count()).sum();
            let tokens = g.usage.total_tokens;
            let prompt_tokens = g.usage.prompt_tokens;
            let completion_tokens = g.usage.completion_tokens;
            let model = g.config.model.clone();
            let cap = g.config.max_iterations;
            let budget = g.config.run_budget_seconds;
            let cap_mode = g.config.cap_enabled;
            drop(g);
            let n_sessions = crate::session::list().map(|l| l.len()).unwrap_or(0);
            app.add_meta(format!("  model:    {model}"));
            app.add_meta(format!("  provider: {}", app.provider));
            app.add_meta(format!("  cap:      {} (Coding Auto-Pilot — /cap to toggle)", if cap_mode { "ON — autonomous" } else { "OFF — waits for your answers" }));
            app.add_meta(format!("  context:  {msgs} msgs · {chars} chars"));
            app.add_meta(format!("  tokens:   {tokens} total ({prompt_tokens} prompt / {completion_tokens} completion)"));
            app.add_meta(format!("  budget:   {cap} iters{}", budget.map(|b| format!(" · {b}s wall")).unwrap_or_default()));
            app.add_meta(format!("  sessions: {n_sessions} saved"));
            app.add_meta(format!("  config:   {}", crate::config::config_dir().join("config.toml").display()));
        }
        "cap" => {
            let mut g = agent.lock().await;
            g.config.cap_enabled = !g.config.cap_enabled;
            let enabled = g.config.cap_enabled;
            drop(g);
            let mut cfg = crate::config::load().unwrap_or_default();
            let _ = crate::config::set_cap(&mut cfg, enabled);
            app.add_meta(format!(
                "  CAP (Coding Auto-Pilot) -> {}",
                if enabled { "ON — full autonomy, no stopping for questions" } else { "OFF — waits for your answer at questions" }
            ));
        }
        "reload" => {
            // Hermes /reload: reload the config into the running session.
            let cfg = crate::config::load();
            match cfg {
                Err(e) => app.add_error(&format!("  reload failed: {e:#}")),
                Ok(cfg) => {
                    let cur_provider = app.provider.clone();
                    let provider = crate::config::resolve_provider(&cfg, Some(&cur_provider), None, None);
                    if provider.name != cur_provider {
                        app.add_meta(format!(
                            "  provider '{}' no longer in config — default is now '{}'",
                            cur_provider,
                            cfg.agent.default_provider
                        ));
                    }
                    let model = crate::config::resolve_model(&cfg, None, &provider);
                    match byteai_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
                        Ok(client) => {
                            {
                                let mut g = agent.lock().await;
                                g.pool.replace_active(&provider.name, client, &model);
                                g.config.model = model.clone();
                                g.config.max_iterations = cfg.agent.max_iterations;
                                g.config.run_budget_seconds = cfg.agent.run_budget_seconds.filter(|&b| b > 0);
                                g.config.warn_ratio = cfg.agent.budget_warn_ratio;
                                if let Some(t) = cfg.agent.tool_timeout_seconds {
                                    g.config.tool_timeout = std::time::Duration::from_secs(t);
                                }
                            }
                            app.provider = provider.name.clone();
                            app.model = model.clone();
                            app.iter_cap = cfg.agent.max_iterations;
                            if let Ok(ids) = agent.lock().await.pool.client().list_models().await {
                                app.models = ids;
                            }
                            app.add_meta(format!(
                                "  reloaded config → provider {}, model {} · {} iters",
                                app.provider, app.model, cfg.agent.max_iterations
                            ));
                        }
                        Err(e) => app.add_error(&format!("  reload: client failed: {e:#}")),
                    }
                }
            }
        }
        "version" | "v" => {
            app.add_meta(format!(
                "  ByteAi v{} — Rust autonomous coding agent",
                env!("CARGO_PKG_VERSION")
            ));
        }
        "copy" => {
            let last = agent
                .lock()
                .await
                .history
                .iter()
                .rev()
                .find_map(|m| match m.role {
                    byteai_types::Role::Assistant => m.content.clone(),
                    _ => None,
                })
                .unwrap_or_default();
            if last.trim().is_empty() {
                app.add_meta("  nothing to copy (no assistant response yet)");
            } else {
                match clipboard_copy(&last) {
                    Ok(msg) => app.add_meta(format!("  {msg} (last response)")),
                    Err(e) => app.add_error(&format!("  copy failed: {e}")),
                }
            }
        }
        "diff" => {
            match git_diff_summary() {
                Ok(lines) => {
                    app.add_meta("  git working tree:");
                    for l in lines {
                        app.add_meta(format!("    {l}"));
                    }
                }
                Err(e) => app.add_error(&format!("  {e}")),
            }
        }
        "usage" => {
            let u = agent.lock().await.usage.clone();
            if parts.get(1).copied() == Some("reset") {
                let mut g = agent.lock().await;
                g.usage = byteai_types::Usage::default();
                app.add_meta("  usage reset");
            } else {
                app.add_meta(format!(
                    "  tokens: {} total ({} prompt / {} completion)",
                    u.total_tokens, u.prompt_tokens, u.completion_tokens
                ));
                app.add_meta("  /usage reset — zero the counters");
            }
        }
        "config" => {
            app.add_meta(format!("  config: {}", crate::config::config_dir().join("config.toml").display()));
            app.add_meta(format!("  data:   {}", crate::config::data_dir().display()));
        }
        "keys" | "keybindings" => {
            app.add_assistant("/keys — ByteAi keybindings (L0–L2 layers)");
            app.add_assistant("┌─ L0: Universal (always visible)");
            app.add_assistant("  Arrow keys     scroll transcript");
            app.add_assistant("  Esc            interrupt turn / clear input / clear card focus");
            app.add_assistant("  Enter          send message / expand focused tool card");
            app.add_assistant("  Ctrl+C         interrupt turn / quit");
            app.add_assistant("┌─ L1: Navigation");
            app.add_assistant("  Ctrl+Shift+K/J scroll up/down");
            app.add_assistant("  Alt+U / Alt+D  page up/down");
            app.add_assistant("  Ctrl+Tab       next model  ·  Ctrl+Shift+Tab  prev model");
            app.add_assistant("  Tab            toggle command mode");
            app.add_assistant("  Shift+Tab      focus/cycle tool cards");
            app.add_assistant("  Mouse wheel    scroll transcript");
            app.add_assistant("  Mouse click    focus/expand tool card");
            app.add_assistant("┌─ L2: Tool-card actions (when a card is focused)");
            app.add_assistant("  Enter (empty)  expand / collapse the focused card");
            app.add_assistant("  c              copy the card's full output to clipboard");
            app.add_assistant("  Shift+Tab      next card");
            app.add_assistant("  Esc            clear card focus");
            app.add_assistant("  (cards are also highlighted in the transcript)");
        }
        "subagent" | "swarm" => {
            let count = if *cmd == "swarm" { 3 } else { 1 };
            let goal = parts.get(1..).unwrap_or(&[]).join(" ");
            if goal.is_empty() {
                app.pending_cmd = Some(cmd.to_string());
                app.pending_label = Some(format!("{cmd} <goal> — spawns {count} subagent(s)"));
                app.add_meta("  type the goal for the subagent(s), then Enter (Esc cancels)");
                return;
            }
            app.add_meta(format!("  spawning {count} subagent(s): {goal}"));
            let g = agent.lock().await;
            let names = g.tools.names();
            let spawn_tool = g.tools.get("spawn");
            drop(g);
            if let Some(spawn_tool) = spawn_tool {
                let args = serde_json::json!({
                    "goals": vec![goal; count],
                    "max_parallel": count as u64,
                });
                let outcome = spawn_tool.execute(args).await;
                app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                app.add_meta(format!("  tools available: {}", names.join(", ")));
            } else {
                app.add_error("spawn tool not available");
            }
        }
        "quit" | "q" | "exit" => {
            app.add_meta("  press Ctrl+C to quit");
        }
        "route" => {
            let task_type = parts.get(1).copied().unwrap_or("chat");
            let task = parts.get(2..).unwrap_or(&[]).join(" ");
            if task.is_empty() {
                app.pending_cmd = Some("route".to_string());
                app.pending_label = Some(format!("route <task> — first word = task type (default {task_type})"));
                app.add_meta("  type the task to route, then Enter (Esc cancels)");
                return;
            }
            app.add_meta(format!("  routing task type '{task_type}'…"));
            let g = agent.lock().await;
            let tool = g.tools.get("route");
            drop(g);
            match tool {
                Some(tool) => {
                    let args = serde_json::json!({"type": task_type, "task": task});
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("route tool not available"),
            }
        }
        "council" => {
            let question = parts.get(1..).unwrap_or(&[]).join(" ");
            if question.is_empty() {
                app.pending_cmd = Some("council".to_string());
                app.pending_label = Some("council <question>".to_string());
                app.add_meta("  type the question for the council, then Enter (Esc cancels)");
                return;
            }
            app.add_meta("  convening council…");
            let g = agent.lock().await;
            let tool = g.tools.get("council");
            drop(g);
            match tool {
                Some(tool) => {
                    let args = serde_json::json!({"question": question});
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("council tool not available"),
            }
        }
        "govern" => {
            let action = parts.get(1..).unwrap_or(&[]).join(" ");
            if action.is_empty() {
                app.pending_cmd = Some("govern".to_string());
                app.pending_label = Some("govern <action to check>".to_string());
                app.add_meta("  type the action to check, then Enter (Esc cancels)");
                return;
            }
            let g = agent.lock().await;
            let tool = g.tools.get("govern");
            drop(g);
            match tool {
                Some(tool) => {
                    let args = serde_json::json!({"action": action});
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("govern tool not available"),
            }
        }
        "ideas" => {
            let focus = parts.get(1..).unwrap_or(&[]).join(" ");
            if focus.is_empty() {
                app.pending_cmd = Some("ideas".to_string());
                app.pending_label = Some("ideas <focus> — e.g. AI, SaaS, developer tools, or a custom search".to_string());
                app.add_meta("  /ideas — type a category or custom search, then Enter (Esc cancels)");
                return;
            }
            let action = if focus == "menu" || focus == "help" { "menu" }
                else if focus.starts_with("research") { "research" }
                else if focus.starts_with("build") { "build" }
                else if focus == "status" { "status" }
                else { "discover" };
            let query = if action == "discover" { focus.clone() }
                else { focus.split_once(' ').map(|x| x.1).unwrap_or("").to_string() };
            let g = agent.lock().await;
            let tool = g.tools.get("ideas");
            drop(g);
            match tool {
                Some(tool) => {
                    let args = serde_json::json!({"action": action, "focus": query});
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("ideas tool not available"),
            }
        }
        "github" => {
            let rest = parts.get(1..).unwrap_or(&[]).join(" ");
            if rest.is_empty() {
                app.pending_cmd = Some("github".to_string());
                app.pending_label = Some("github <skills|harnesses|tools|mcp|improve|current|evaluate repo|search ...>".to_string());
                app.add_meta("  /github — type a target + query, then Enter (Esc cancels)");
                return;
            }
            let first = parts.get(1).copied().unwrap_or("").to_string();
            let action = match first.as_str() {
                "menu"|"search"|"evaluate"|"improve"|"current"|"memory"|"graph"|"connect"|"status"|"push" => first.clone(),
                _ => "search".to_string(),
            };
            let query = rest.clone();
            let g = agent.lock().await;
            let tool = g.tools.get("github");
            drop(g);
            match tool {
                Some(tool) => {
                    let args = serde_json::json!({"action": action, "target": first, "query": query});
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("github tool not available"),
            }
        }
        "setup" => {
            app.add_meta("  /setup — ByteAI's interactive setup wizard");
            app.add_assistant("To run the interactive setup wizard (providers, models, skills, tools):");
            app.add_assistant("  1. Open a separate terminal");
            app.add_assistant("  2. Run:  byteai setup");
            app.add_assistant("  The wizard walks you through provider config, agent settings,");
            app.add_assistant("  skills, and verification step by step.");
        }
        "goal" => {
            let action = parts.get(1).copied().unwrap_or("get");
            let text = parts.get(2..).unwrap_or(&[]).join(" ");
            let args = serde_json::json!({"action": action, "text": text});
            let g = agent.lock().await;
            let tool = g.tools.get("goal");
            drop(g);
            match tool {
                Some(tool) => {
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("goal tool not available"),
            }
        }
        "terminal" => {
            let action = parts.get(1).copied().unwrap_or("list");
            let args = if action == "run" {
                let id = parts.get(2).copied().unwrap_or("");
                let command = parts.get(3..).unwrap_or(&[]).join(" ");
                serde_json::json!({"action": "run", "id": id, "command": command})
            } else {
                let label = parts.get(2).copied().unwrap_or("");
                serde_json::json!({"action": action, "label": label})
            };
            let g = agent.lock().await;
            let tool = g.tools.get("terminal");
            drop(g);
            match tool {
                Some(tool) => {
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("terminal tool not available"),
            }
        }
        "feedback" => {
            let action = parts.get(1).copied().unwrap_or("stats");
            let args = if action == "rate" {
                let id = parts.get(2).copied().unwrap_or("");
                let score = parts.get(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                let note = parts.get(4..).unwrap_or(&[]).join(" ");
                serde_json::json!({"action": "rate", "msg_id": id, "score": score, "text": note})
            } else {
                let text = parts.get(2..).unwrap_or(&[]).join(" ");
                serde_json::json!({"action": action, "text": text})
            };
            let g = agent.lock().await;
            let tool = g.tools.get("feedback");
            drop(g);
            match tool {
                Some(tool) => {
                    let outcome = tool.execute(args).await;
                    app.add_tool_card(&outcome.name, outcome.ok, outcome.elapsed_ms, &outcome.output);
                }
                None => app.add_error("feedback tool not available"),
            }
        }
        other => {
            app.add_error(&format!("unknown command /{other} — try /help"));
        }
    }
}

/// Commands whose name starts with `prefix` (after the leading `/`).
fn matching_commands(prefix: &str) -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, _)| *name)
        .collect()
}

/// Compute the visible window into a sorted command list for the palette.
/// Returns `(offset, visible_count)` so the palette draws
/// `matches[offset..offset + visible_count]`, with the selection at `sel`
/// kept on-screen.
fn palette_window(total: usize, sel: usize, visible: usize) -> (usize, usize) {
    let visible = visible.max(1);
    let offset = if total > visible && sel >= visible {
        sel - visible + 1
    } else {
        0
    };
    (offset, visible.min(total.saturating_sub(offset)))
}

fn draw(f: &mut Frame, app: &mut App) {
    // Advance spinner while busy (called every frame).
    if app.busy {
        app.spinner_frame = app.spinner_frame.wrapping_add(1);
    }
    let area = f.area();
    let palette_h = if app.is_command && !app.input.is_empty() {
        let n = matching_commands(&app.input[1..]).len();
        if n > 0 {
            // border + up to 10 items (+1 hint row when more match than fit)
            n.min(10) as u16 + 1 + if n > 10 { 1 } else { 0 }
        } else {
            0
        }
    } else if app.picker.is_some() {
        // Picker window: border + title + up to 12 items + hint row.
        let n = app.picker.as_ref().map(|p| p.items.len()).unwrap_or(0);
        (n.min(12) as u16) + 2 + if n > 12 { 1 } else { 0 }
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(palette_h),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    draw_chat(f, chunks[1], app);
    if let Some(p) = &app.picker {
        draw_picker(f, chunks[2], p);
    } else if palette_h > 0 {
        draw_palette(f, chunks[2], app);
    }
    draw_input(f, chunks[3], app);
    draw_status(f, chunks[4], app);
}

/// Count installed skills by scanning `<data_dir>/skills/` for SKILL.md files.
/// Uses the same scanner as the `skills` tool so the header count always
/// matches what `skills list` reports.
fn count_skills(data_dir: &std::path::Path) -> usize {
    let mut metas = Vec::new();
    byteai_tools::skills::scan_dir(&data_dir.join("skills"), &mut metas, 0);
    metas.len()
}

fn draw_header(f: &mut Frame, area: Rect, app: &mut App) {
    let header = Line::from(vec![
        Span::styled(" ByteAi ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("· "),
        Span::styled(&app.model, Style::default().fg(Color::White)),
        Span::raw(" · "),
        Span::styled(&app.provider, Style::default().fg(Color::Yellow)),
        Span::raw(" · "),
        Span::styled(format!("{} tools", app.tools_count), Style::default().fg(Color::Gray)),
        Span::raw(" · "),
        Span::styled(format!("{} skills", app.skills_count), Style::default().fg(Color::Gray)),
    ]);
    f.render_widget(Paragraph::new(header).style(Style::default().bg(Color::Black)), area);
}

/// Dedicated live-status row, always rendered under the header so it can
/// never be truncated or scrolled away (unlike appending to the header line).
/// Shows exactly what the agent is doing right now: phase icon + per-phase
/// spinner + active tool + iteration progress (Hermes-style).
/// Collapse an error/reason into a single-line, bounded note for the
/// live-status row: strip newlines and control chars, cap the length so a
/// raw provider error (e.g. a JSON body) can never blow out the 1-row banner.
fn sanitize_note(text: &str) -> String {
    const MAX: usize = 96;
    let mut s = String::with_capacity(text.len().min(MAX + 8));
    for ch in text.chars() {
        match ch {
            '\n' | '\r' | '\t' => s.push(' '),
            c if c.is_control() => {}
            c => s.push(c),
        }
    }
    let mut s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > MAX {
        s = s.chars().take(MAX.saturating_sub(1)).collect();
        s.push('…');
    }
    s
}

/// Build every chat line exactly as the Paragraph renders it, plus a
/// parallel map of which content row belongs to which tool card (None for
/// non-card rows). Shared by draw_chat (rendering) and the mouse handler
/// (click → card mapping) so the two can never drift apart.
fn build_chat_lines(app: &App) -> (Vec<Line<'static>>, Vec<Option<u64>>) {
    let mut lines: Vec<Line> = Vec::new();
    let mut card_map: Vec<Option<u64>> = Vec::new();
    for entry in &app.log {
        match entry {
            LogEntry::Text { content, style } => {
                lines.push(Line::from(Span::styled(content.clone(), *style)));
                card_map.push(None);
            }
            LogEntry::Assistant(content) => {
                lines.push(Line::from(Span::styled(
                    content.clone(),
                    Style::default().fg(Color::White),
                )));
                card_map.push(None);
            }
            LogEntry::ToolCard { id, name, ok, elapsed_ms, output, truncated } => {
                let sigil = toolcards::tool_sigil(name);
                let (status, color) = if *ok { ("✓", Color::Green) } else { ("✗", Color::Red) };
                let focused = Some(*id) == app.card_focus;
                let expanded = app.card_expanded.contains(id);

                // Gutter marker (▸) for the focused card.
                let gutter = if focused { "▸" } else { " " };
                let head_style = if focused {
                    Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(color).add_modifier(Modifier::BOLD)
                };
                let mut head_spans = vec![
                    Span::styled(format!(" {gutter}"), Style::default().fg(color)),
                    Span::styled(format!("{sigil} "), Style::default().fg(color)),
                    Span::styled(name.clone(), head_style),
                    Span::raw(" "),
                    Span::styled(status, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Span::raw(" "),
                    Span::styled(
                        toolcards::fmt_elapsed(*elapsed_ms),
                        Style::default().fg(if focused { Color::White } else { Color::DarkGray }).add_modifier(Modifier::DIM),
                    ),
                ];
                // Size chip
                if !output.is_empty() {
                    head_spans.push(Span::raw(" "));
                    head_spans.push(Span::styled(
                        format!("· {}", toolcards::fmt_bytes(output.len())),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                    ));
                }
                // Focus hint (L2 action mnemonics: Enter expand, c copy)
                if focused {
                    let hint = if expanded {
                        "  [Enter collapse · c copy]"
                    } else {
                        "  [Enter expand · c copy · ⇧Tab next]"
                    };
                    head_spans.push(Span::styled(
                        hint,
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                    ));
                }
                lines.push(Line::from(head_spans));
                card_map.push(Some(*id));

                if expanded {
                    for line in output.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("   {line}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                        card_map.push(Some(*id));
                    }
                    if *truncated {
                        lines.push(Line::from(Span::styled(
                            "   … (output truncated)",
                            Style::default().fg(Color::Yellow).add_modifier(Modifier::DIM),
                        )));
                        card_map.push(Some(*id));
                    }
                } else {
                    // Smart preview: first 3 non-empty lines, capped at ~100 chars
                    let p = toolcards::preview(output, 3, 100);
                    for pline in &p.lines {
                        lines.push(Line::from(Span::styled(
                            format!("   {pline}"),
                            Style::default().fg(Color::DarkGray),
                        )));
                        card_map.push(Some(*id));
                    }
                    // Footer: what's hidden
                    let hidden = p.total_lines.saturating_sub(p.lines.len())
                        + if *truncated { 1 } else { 0 };
                    let hidden_bytes = p.total_bytes.saturating_sub(p.shown_bytes);
                    if hidden > 0 {
                        let mut foot = format!("   └ … +{hidden} line{}", if hidden == 1 { "" } else { "s" });
                        if hidden_bytes > 0 {
                            foot.push_str(&format!(" · +{}", toolcards::fmt_bytes(hidden_bytes)));
                        }
                        foot.push_str(" — Enter to expand");
                        lines.push(Line::from(Span::styled(foot, Style::default().fg(Color::DarkGray))));
                        card_map.push(Some(*id));
                    } else if output.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "   (no output)",
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                        )));
                        card_map.push(Some(*id));
                    }
                }
            }
            LogEntry::Meta(text) => {
                lines.push(Line::from(Span::styled(
                    text.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
                card_map.push(None);
            }
        }
    }

    // Live activity status appears right in the chat while the agent is
    // working, so the user sees exactly what it is doing at the answer
    // point (Hermes-style): phase-specific icon + spinner + active tool.
    if app.busy {
        let secs = app.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(app.last_run_duration);
        let live_line = match app.live.as_ref().and_then(|l| l.try_lock().ok()) {
            Some(l) => l.line(app.spinner_frame),
            None => String::new(),
        };
        let label = if live_line.is_empty() {
            format!("{secs}s working…")
        } else {
            format!("{live_line} · {secs}s")
        };
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default().fg(Color::DarkGray)),
            Span::styled(label, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]));
        card_map.push(None);
    }

    (lines, card_map)
}

fn draw_chat(f: &mut Frame, area: Rect, app: &mut App) {
    let max_rows = area.height as usize;

    let (lines, _card_map) = build_chat_lines(app);

    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    // EXACT wrapped row count (same wrapping ratatui uses to render), so the
    // scroll pin is always precise. A char-based div_ceil estimate drifts
    // when long words / URLs / markdown wrap to more rows than the estimate
    // predicts — that pushed new answers to the TOP of the chat box with a
    // blank gap below.
    let total_rows = paragraph.line_count(area.width);

    // Scroll offset (rows skipped from the top). follow_bottom pins to the
    // newest row; manual scroll-up moves `app.scroll` rows back toward the
    // top (offset = at_bottom - scroll, never below 0).
    let at_bottom = total_rows.saturating_sub(max_rows);
    if !app.follow_bottom {
        // Keep the view anchored to the same absolute content when new rows
        // arrive below while the user is scrolled up (don't drift toward
        // newer content).
        let growth = at_bottom.saturating_sub(app.max_scroll);
        app.scroll = app.scroll.saturating_add(growth);
    }
    // Remember how far the user can scroll up (total scrollable rows). This
    // is a row count, NOT an entry count — capped correctly so long answers
    // can always be scrolled all the way back to the top.
    app.max_scroll = at_bottom;

    let paragraph = paragraph.scroll(((chat_offset(total_rows, max_rows, app.follow_bottom, app.scroll)) as u16, 0));
    f.render_widget(paragraph, area);

    // Remember chat geometry for mouse → card mapping.
    app.last_chat_y = area.y;
    app.last_chat_h = area.height;
    app.last_chat_w = area.width;
}

/// Compute the Paragraph scroll offset (rows skipped from the top) for the
/// chat transcript. `total_rows` = estimated wrapped rows of all content,
/// `max_rows` = visible chat height. follow_bottom pins to the newest row;
/// manual scroll moves `scroll` rows back toward the top. The result is
/// clamped so it can never blank the view (offset > content) or go negative.
fn chat_offset(total_rows: usize, max_rows: usize, follow_bottom: bool, scroll: usize) -> usize {
    let at_bottom = total_rows.saturating_sub(max_rows);
    let offset = if follow_bottom {
        at_bottom
    } else {
        at_bottom.saturating_sub(scroll)
    };
    offset.min(total_rows)
}

/// Command palette shown while typing `/` (jcode-style autocomplete).
fn draw_palette(f: &mut Frame, area: Rect, app: &mut App) {
    let prefix = &app.input[1..];
    let matches = matching_commands(prefix);
    if matches.is_empty() {
        return;
    }
    // Keep the highlighted row on-screen even when more commands match than
    // fit in the palette (Up/Down scroll the window, jcode-style).
    let content_rows = area.height.saturating_sub(1) as usize; // minus border row
    let show_hint = matches.len() > content_rows;
    // Reserve one row for the "↑↓ scroll" hint when there's overflow.
    let visible = if show_hint {
        content_rows.saturating_sub(1).max(1)
    } else {
        content_rows
    };
    let (offset, count) = palette_window(matches.len(), app.palette_idx, visible);
    let sel = app.palette_idx.min(matches.len() - 1);

    let mut lines: Vec<Line> = Vec::new();
    for (k, name) in matches.iter().enumerate().skip(offset).take(count) {
        let desc = COMMANDS.iter().find(|(n, _)| n == name).map(|(_, d)| *d).unwrap_or("");
        let selected = k == sel;
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let label = if selected {
            format!(" ▸ /{name:<9} {desc}")
        } else {
            format!("   /{name:<9} {desc}")
        };
        lines.push(Line::from(Span::styled(label, style)));
    }
    // Footer hint row when there are more matches than fit (scrollable).
    if show_hint {
        lines.push(Line::from(Span::styled(
            format!("   ↑↓ scroll · {} commands match", matches.len()),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .title(Span::styled(" commands ", Style::default().fg(Color::Cyan)));
    let list = List::new(lines).block(block);
    f.render_widget(list, area);
}

/// Interactive picker (models, providers, sessions, gate ledgers).
/// Renders a scrollable list with title; Enter selects, Esc cancels.
fn draw_picker(f: &mut Frame, area: Rect, pick: &Picker) {
    let n = pick.items.len();
    let max = area.height.saturating_sub(2) as usize; // minus border
    let show_hint = n > max;
    let visible = if show_hint { max.max(1) } else { max };
    let (offset, count) = palette_window(n, pick.sel, visible);
    let sel = pick.sel.min(n.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::new();
    for (k, item) in pick.items.iter().enumerate().skip(offset).take(count) {
        let selected = k == sel;
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let label = if selected { format!(" ▸ {item}") } else { format!("   {item}") };
        lines.push(Line::from(Span::styled(label, style)));
    }
    if show_hint {
        lines.push(Line::from(Span::styled(
            format!("   ↑↓ scroll · {n} items"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .title(Span::styled(
            format!(" {} ", pick.title),
            Style::default().fg(Color::Cyan),
        ));
    let list = List::new(lines).block(block);
    f.render_widget(list, area);
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let prefix = if app.is_command { " / " } else if app.pending_cmd.is_some() { "  > " } else { "> " };
    let display = format!("{prefix}{}", app.input);

    // Hard-wrap the display at the inner box width: ratatui's soft-wrap
    // wraps at WORD boundaries (so "> " + long word became two lines);
    // a terminal input box must wrap at CHARACTER boundaries instead.
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let chars: Vec<char> = display.chars().collect();
    let mut hard_wrapped: Vec<String> = Vec::new();
    for chunk in chars.chunks(inner_w) {
        hard_wrapped.push(chunk.iter().collect::<String>());
    }
    let text = hard_wrapped.join("\n");

    // Total wrapped rows and how many to skip so the LAST line (where the
    // cursor is) stays visible inside the box.
    let total_rows = hard_wrapped.len().max(1);
    let visible_rows = area.height.saturating_sub(2).max(1) as usize;
    let scroll = total_rows.saturating_sub(visible_rows);

    let input = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default())
                .title(Span::styled(
                    app.pending_label.as_deref().unwrap_or(""),
                    Style::default().fg(Color::Yellow),
                )),
        )
        .scroll((scroll as u16, 0))
        .style(Style::default().fg(Color::White));
    f.render_widget(input, area);

    // Cursor at the end of input, on the last visible wrapped row.
    let len = chars.len();
    let col = len % inner_w;
    let cur_line = (len / inner_w).saturating_sub(scroll);
    let cursor_x = area.x + 1 + col as u16;
    let cursor_y = area.y + 1 + cur_line as u16;
    f.set_cursor_position((cursor_x, cursor_y));
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    // Contextual tool-card hint (L2 actions): when a card is focused show the
    // expand/copy actions; otherwise remind that cards exist and how to reach
    // them (keyboard + mouse).
    let card_hint = if app.card_focus.is_some() {
        " ▸ [Enter] expand · [c] copy · ⇧Tab next · Esc exit"
    } else if !app.card_ids().is_empty() {
        " ⇧Tab / click a card to focus"
    } else {
        ""
    };
    let status = format!(
        "  model: {} | provider: {} | tokens: {} | Ctrl+Shift+K/J scroll | Ctrl+C quit{}",
        app.model, app.provider, app.last_tokens, card_hint
    );
    let status_widget = Paragraph::new(Text::from(Line::from(Span::styled(
        status,
        Style::default().fg(Color::DarkGray),
    ))))
    .style(Style::default().bg(Color::Black));
    f.render_widget(status_widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_note_bounds_and_flattens() {
        // Long multi-line provider error → single line, capped, with ellipsis.
        let long = format!("RATE_LIMIT: chat completions returned 400 Bad Request: {{\"error\":{{\"message\":\"credit insufficient balance {}\"}}}}", "x".repeat(400));
        let s = sanitize_note(&long);
        assert!(!s.contains('\n') && !s.contains('\r') && !s.contains('\t'), "no newlines: {s:?}");
        assert!(s.chars().count() <= 96, "capped at 96, got {}", s.chars().count());
        assert!(s.ends_with('…'), "ends with ellipsis: {s:?}");
        assert!(s.starts_with("RATE_LIMIT:"), "reason prefix preserved: {s:?}");

        // Short note passes through untouched (whitespace-collapsed).
        let s2 = sanitize_note("shell  exited  1\n");
        assert_eq!(s2, "shell exited 1");
        assert!(!s2.ends_with('…'));

        // Control chars stripped, not kept.
        let s3 = sanitize_note("bad\x01\x02token");
        assert_eq!(s3, "badtoken");
    }

    /// Every command in the palette must be handled by handle_command.
    #[test]
    fn every_palette_command_is_handled() {
        let handled = [
            "help", "status", "new", "retry", "undo", "compress", "title",
            "model", "provider", "addprovider", "reload", "tools",
            "clear", "save", "usage", "session", "resume", "config", "keys",
            "version", "copy", "diff",
            "subagent", "swarm", "route", "council", "govern", "gates", "cap",
            "ideas", "github", "goal", "terminal", "feedback", "setup", "dan", "quit",
        ];
        for (name, _) in COMMANDS {
            assert!(handled.contains(name), "palette command /{name} has no handler");
        }
        // Every handled command must be reachable from the palette too.
        for name in handled {
            assert!(
                COMMANDS.iter().any(|(n, _)| *n == name),
                "handler for /{name} missing from COMMANDS palette"
            );
        }
    }

    #[test]
    fn rebuild_log_roundtrips_history() {
        let mut app = App::new("m1".into(), "p1".into(), 3, 0);
        let hist = vec![
            byteai_types::Message::system("sys"),
            byteai_types::Message::user("hello"),
            byteai_types::Message::assistant(Some("hi back".into()), None, None),
            byteai_types::Message::user("again"),
        ];
        rebuild_log_from_history(&mut app, &hist);
        // banner (5 lines + meta) + 2 user + 2 assistant lines
        let users = app.log.iter().filter(|e| matches!(e, LogEntry::Text { .. })).count();
        let asst = app.log.iter().filter(|e| matches!(e, LogEntry::Assistant(_))).count();
        assert_eq!(asst, 2, "assistant lines restored");
        assert!(users >= 2, "user lines restored: {users}");
        // The user lines carry the "❯" echo prefix.
        assert!(app.log.iter().any(|e| matches!(e, LogEntry::Text { content, .. } if content.contains("hello"))));
    }

    #[test]
    fn git_diff_summary_reports_clean_tree() {
        // In a real git repo, it returns status lines; in a non-repo it errors.
        match git_diff_summary() {
            Ok(lines) => assert!(!lines.is_empty()),
            Err(e) => assert!(e.contains("not a git"), "unexpected error: {e}"),
        }
    }

    #[test]
    fn command_debounce_rejects_rapid_repeat() {
        let mut app = App::new("m1".into(), "p1".into(), 3, 0);
        // First run: accepted, timestamp recorded.
        let fresh = app
            .last_cmd
            .as_ref()
            .map(|(c, t)| !(c == "help" && t.elapsed() < std::time::Duration::from_millis(300)))
            .unwrap_or(true);
        assert!(fresh);
        app.last_cmd = Some(("help".to_string(), std::time::Instant::now()));
        // Immediate repeat within 300ms: rejected (the double-output guard).
        let repeat = app
            .last_cmd
            .as_ref()
            .map(|(c, t)| !(c == "help" && t.elapsed() < std::time::Duration::from_millis(300)))
            .unwrap_or(true);
        assert!(!repeat, "rapid identical repeat must be debounced");
        // A different command is never debounced.
        let other = app
            .last_cmd
            .as_ref()
            .map(|(c, t)| !(c == "models" && t.elapsed() < std::time::Duration::from_millis(300)))
            .unwrap_or(true);
        assert!(other, "different command must not be debounced");
    }

    #[test]
    fn command_names_are_unique() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        for w in names.windows(2) {
            assert_ne!(w[0], w[1], "duplicate command /{}", w[0]);
        }
    }

    #[test]
    fn find_gate_files_discovers_ledgers() {
        let dir = std::env::temp_dir().join(format!("byteai-gates-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".unlazy/scope1/gates")).unwrap();
        std::fs::create_dir_all(dir.join(".unlazy/scope2")).unwrap();
        std::fs::write(dir.join("GATES.md"), "# Gates: root\n").unwrap();
        std::fs::write(dir.join(".unlazy/scope1/gates/leaf-1.1.md"), "# Gates: leaf\n").unwrap();
        std::fs::write(dir.join(".unlazy/scope1/gates/leaf-1.2.md"), "# Gates: leaf2\n").unwrap();
        std::fs::write(dir.join(".unlazy/scope2/GATES.md"), "# Gates: plan\n").unwrap();
        let found = find_gate_files(dir.to_str().unwrap());
        assert_eq!(found.len(), 4, "found: {found:?}");
        assert!(found.iter().any(|p| p.ends_with("GATES.md")), "{found:?}");
        assert!(found.iter().any(|p| p.contains("leaf-1.1")), "{found:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn picker_selection_clamps() {
        let mut app = App::new("m1".into(), "p1".into(), 3, 0);
        app.picker = Some(Picker {
            title: "t".into(),
            items: vec!["a".into(), "b".into(), "c".into()],
            values: vec!["a".into(), "b".into(), "c".into()],
            sel: 0,
            action: PickAction::SetModel { provider: "p1".into() },
        });
        // Simulate Down×10: clamps to last item.
        for _ in 0..10 {
            if let Some(p) = app.picker.as_mut() {
                let n = p.items.len().max(1);
                p.sel = (p.sel + 1).min(n - 1);
            }
        }
        assert_eq!(app.picker.as_ref().unwrap().sel, 2);
        // Simulate Up×10: clamps to first item.
        for _ in 0..10 {
            if let Some(p) = app.picker.as_mut() {
                p.sel = p.sel.saturating_sub(1);
            }
        }
        assert_eq!(app.picker.as_ref().unwrap().sel, 0);
    }

    #[test]
    fn matching_commands_prefixes() {
        // Empty prefix matches everything.
        assert_eq!(matching_commands("").len(), COMMANDS.len());
        // Unique prefix.
        assert_eq!(matching_commands("prov"), vec!["provider"]);
        assert_eq!(matching_commands("hel"), vec!["help"]);
        // Prefix matching one command only (models was merged into model).
        assert_eq!(matching_commands("mo"), vec!["model"]);
        // No match.
        assert!(matching_commands("zzz").is_empty());
    }

    #[test]
    fn palette_window_keeps_selection_visible() {
        // All fit: no offset.
        assert_eq!(palette_window(5, 3, 10), (0, 5));
        // More than visible: offset moves so `sel` stays on screen.
        assert_eq!(palette_window(17, 0, 10), (0, 10));
        assert_eq!(palette_window(17, 9, 10), (0, 10)); // last visible slot
        assert_eq!(palette_window(17, 10, 10), (1, 10)); // scrolls down one
        assert_eq!(palette_window(17, 16, 10), (7, 10)); // bottom
        // visible clamps to total.
        assert_eq!(palette_window(3, 2, 10), (0, 3));
        // Degenerate cases never panic.
        assert_eq!(palette_window(0, 0, 0), (0, 0));
        assert_eq!(palette_window(1, 0, 0), (0, 1));
    }

    #[test]
    fn command_descriptions_present() {
        for (name, desc) in COMMANDS {
            assert!(!desc.is_empty(), "/{name} has an empty description");
        }
    }

    // --- Tool-card interactivity: focus cycling, expand/collapse, mouse map ---

    #[test]
    fn build_chat_lines_maps_card_rows() {
        let mut app = App::new("m1".into(), "mock".into(), 3, 0);
        app.add_tool_card("shell", true, 5, "line1\nline2\nline3\nline4\nline5");
        let (lines, card_map) = build_chat_lines(&app);
        // Header line carries the sigil + name; every card row maps to an id.
        assert!(lines.iter().any(|l| l.to_string().contains("❯ shell")), "sigil header missing");
        let some_ids: Vec<u64> = card_map.iter().filter_map(|c| *c).collect();
        assert!(!some_ids.is_empty(), "card rows must map to an id");
        // All Some entries share the SAME card id.
        assert!(some_ids.windows(2).all(|w| w[0] == w[1]), "one card → one id");
        // Preview shows 3 of the 5 lines, footer reports the hidden 2.
        let rendered = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("+2 line"), "hidden-line footer: {rendered}");
    }

    #[test]
    fn card_focus_cycles_and_toggles() {
        let mut app = App::new("m1".into(), "mock".into(), 3, 0);
        app.add_tool_card("shell", true, 5, "out1");
        app.add_tool_card("memory", true, 3, "out2");
        app.add_tool_card("todo", true, 1, "out3");
        assert!(app.card_focus.is_none());
        // Shift+Tab (dir -1) focuses the LAST card; then cycles backwards.
        app.cycle_card_focus(-1);
        let ids = app.card_ids();
        assert_eq!(app.card_focus, Some(ids[2]));
        app.cycle_card_focus(-1);
        assert_eq!(app.card_focus, Some(ids[1]));
        app.cycle_card_focus(1);
        assert_eq!(app.card_focus, Some(ids[2]));
        app.cycle_card_focus(2); // (2+2) mod 3 = 1 → wraps forward
        assert_eq!(app.card_focus, Some(ids[1]));
        // Enter (toggle) expands, then collapses.
        app.toggle_card();
        assert!(app.card_expanded.contains(&ids[1]), "toggle expands");
        app.toggle_card();
        assert!(!app.card_expanded.contains(&ids[1]), "toggle collapses");
        // Clearing focus never panics.
        app.card_focus = None;
        app.toggle_card();
        assert!(app.card_focus.is_none());
    }

    #[test]
    fn mouse_row_map_respects_wrapping_and_scroll() {
        // Simulate the geometry draw_chat stores, then verify a click on a
        // card row resolves to that card via the same offset math handle_mouse
        // uses.
        let mut app = App::new("m1".into(), "mock".into(), 3, 0);
        app.add_user("hello");
        app.add_tool_card("shell", true, 5, "a\nb\nc");
        app.add_assistant("answer");
        let (lines, card_map) = build_chat_lines(&app);
        let mut row_map: Vec<Option<u64>> = Vec::new();
        for (line, card) in lines.iter().zip(card_map.iter()) {
            let rows = Paragraph::new(line.clone())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .line_count(120)
                .max(1);
            for _ in 0..rows {
                row_map.push(*card);
            }
        }
        let id = app.card_ids()[0];
        // The card occupies several rows; all of them map back to its id.
        assert!(row_map.iter().filter(|c| **c == Some(id)).count() >= 2);
        // Click the FIRST card row with follow_bottom + a tall viewport: the
        // offset is 0, so the click resolves straight to that card's id.
        let first_card_row = row_map.iter().position(|c| *c == Some(id)).unwrap();
        let offset = chat_offset(row_map.len(), 50, true, 0);
        let clicked = row_map.get(offset + first_card_row).copied().flatten();
        assert_eq!(clicked, Some(id), "click must resolve to the card under the cursor");
        let _ = app;
    }

    // --- Chat transcript scrolling (the "answers disappear" regression) ---
    // The offset must never blank the view: it must stay within [0, total_rows]
    // and always subtract (never add) the manual scroll distance.

    #[test]
    fn chat_offset_pins_to_bottom_when_following() {
        // follow_bottom: newest content at the bottom, nothing scrolled.
        assert_eq!(chat_offset(50, 23, true, 0), 27); // 50 rows, 23 visible
        assert_eq!(chat_offset(10, 23, true, 0), 0); // fits: no offset
        assert_eq!(chat_offset(0, 23, true, 0), 0);
    }

    #[test]
    fn chat_offset_scrolls_up_by_subtracting() {
        // Scrolled 1..N rows up from the bottom: offset shrinks toward 0.
        let total = 50;
        let rows = 23;
        let at_bottom = 27;
        assert_eq!(chat_offset(total, rows, false, 1), at_bottom - 1);
        assert_eq!(chat_offset(total, rows, false, 10), at_bottom - 10);
        // All the way up: reach the very top, never below 0.
        assert_eq!(chat_offset(total, rows, false, at_bottom), 0);
        assert_eq!(chat_offset(total, rows, false, at_bottom + 999), 0);
    }

    #[test]
    fn chat_offset_never_blanks_with_large_entry_scroll() {
        // Regression: the old code capped the manual scroll at log entry count
        // (e.g. 455 entries after a 450-line answer) and ADDED it to the bottom
        // offset: 892 + 455 = 1347, clamped to total_rows=915 -> blank chat.
        // The new math subtracts and clamps to [0, total_rows], so content is
        // always visible and the top is reachable.
        let total_rows = 915; // long answer wrapped rows
        let max_rows = 23;
        let at_bottom = 915 - 23; // 892
        // Even an absurd scroll distance can never push past the content.
        assert!(chat_offset(total_rows, max_rows, false, 455) < total_rows);
        assert_eq!(chat_offset(total_rows, max_rows, false, at_bottom), 0);
        // follow_bottom on the same content still lands exactly at the bottom.
        assert_eq!(chat_offset(total_rows, max_rows, true, 0), at_bottom);
    }

    #[test]
    fn chat_offset_degrades_gracefully() {
        // Empty or tiny transcripts never panic or go negative.
        assert_eq!(chat_offset(0, 0, false, 0), 0);
        assert_eq!(chat_offset(0, 23, false, 5), 0);
        assert_eq!(chat_offset(3, 0, true, 0), 3); // no room: offset = total
        assert_eq!(chat_offset(3, 0, false, 999), 0); // still clamped in-bounds
    }

    // --- Turn interruption (Esc / Ctrl+C) ---

    #[test]
    fn interrupt_turn_resets_state_and_annotates() {
        let mut app = App::new("m1".into(), "mock".into(), 27, 0);
        app.busy = true;
        app.busy_since = Some(std::time::Instant::now());
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        app.turn_rx = Some(rx);
        app.pending_queue.push("queued question".to_string());
        let before = app.log.len();

        interrupt_turn(&mut app, "Esc");

        assert!(!app.busy, "busy must clear");
        assert!(app.busy_since.is_none(), "busy timer must clear");
        assert!(app.turn_rx.is_none(), "stream channel must drop");
        // Queued prompts are PRESERVED: the user typed them deliberately and
        // interrupt only stops the current turn, not their follow-ups.
        assert_eq!(app.pending_queue.len(), 1, "queued prompts survive interrupt");
        assert_eq!(app.log.len(), before + 1, "one meta line added");
        assert!(
            app.log.iter().any(|e| matches!(
                e,
                LogEntry::Meta(m) if m.contains("interrupted (Esc)")
            )),
            "interrupt must be annotated in the transcript"
        );
    }

    // --- Exact wrapped-row measurement (the "answers drift to the top" bug) ---
    // The old char-based div_ceil estimate UNDERCOUNTS rows when long words /
    // URLs wrap to more rows than the character math predicts, so the scroll
    // pin pointed above the real bottom and new answers appeared at the top
    // of the chat box with blank space below. Paragraph::line_count is the
    // exact count ratatui renders with, so the pin is always precise.

    fn para_rows(text: &str, width: u16) -> (usize, usize) {
        let old_estimate = text
            .lines()
            .map(|l| l.chars().count().div_ceil(width as usize).max(1))
            .sum::<usize>()
            .max(1);
        let exact = ratatui::widgets::Paragraph::new(text.to_string())
            .wrap(ratatui::widgets::Wrap { trim: false })
            .line_count(width);
        (old_estimate, exact)
    }

    #[test]
    fn long_words_wrap_to_more_rows_than_char_estimate() {
        // Empirically-proven divergence: whole-word packing reserves rows that
        // char-based div_ceil misses, so word wrap renders MORE rows.
        // est=4 (ceil(76/20)) but word-wrap needs 5 rows at width 20.
        let text = "short short short short longlonglonglonglonglonglonglonglonglonglonglong tail";
        let (est, exact) = para_rows(text, 20);
        assert_eq!(est, 4);
        assert_eq!(exact, 5, "word wrap must need more rows than char math");
        // Same for irregular word lengths at width 21.
        let text = "aaaaaaaaaa bbbbbbbbbbbbbbbbbb cccccccc dddddddddd eee ffffffffffffffff gggggggggg";
        let (est, exact) = para_rows(text, 21);
        assert_eq!(est, 4);
        assert_eq!(exact, 5, "mixed word lengths must diverge too");
    }

    #[test]
    fn notify_writer_forwards_lines_into_channel() {
        // Tracing capture: a formatted line written to the custom writer must
        // land in the notification channel (trimmed), NOT on stderr.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut w = NotifyWriter(tx);
        let line = b"WARN chat_stream transient error 429 Too Many Requests on attempt 3/4; retrying in 4s\n";
        io::Write::write_all(&mut w, line).unwrap();
        let msg = rx.try_recv().expect("line must be forwarded");
        assert_eq!(msg, "WARN chat_stream transient error 429 Too Many Requests on attempt 3/4; retrying in 4s");
        assert!(rx.try_recv().is_err(), "one event → one notification");
    }

    #[test]
    fn notify_drain_renders_as_meta_notification() {
        // The TUI drains captured notifications into the chat log as Meta
        // entries (the response area) — never into the input box.
        let mut app = App::new("m1".into(), "mock".into(), 27, 0);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        app.notify_rx = Some(rx);
        tx.send("WARN chat_stream transient error 429 on attempt 3/4; retrying in 4s".into()).unwrap();
        app.drain();
        assert!(app.notify_rx.is_some(), "channel must stay attached");
        assert!(
            app.log.iter().any(|e| matches!(
                e,
                LogEntry::Meta(m) if m.contains("429") && m.contains("retrying in 4s")
            )),
            "notification must appear in the chat log"
        );
    }

    #[test]
    fn dan_topic_fuzzy_matching_is_easy() {
        // Exact topics resolve.
        assert_eq!(match_dan_topic("planning"), Some("planning"));
        assert_eq!(match_dan_topic("security"), Some("security"));
        // Unique prefixes resolve.
        assert_eq!(match_dan_topic("multi"), Some("multi_agent"));
        assert_eq!(match_dan_topic("sandbox"), Some("agent_sandboxes"));
        assert_eq!(match_dan_topic("context"), Some("context_engineering"));
        assert_eq!(match_dan_topic("harness"), Some("harness_engineering"));
        assert_eq!(match_dan_topic("local"), Some("local_models"));
        assert_eq!(match_dan_topic("skills"), Some("skills_system"));
        // Spaces normalize to underscores.
        assert_eq!(match_dan_topic("context eng"), Some("context_engineering"));
        assert_eq!(match_dan_topic("model selection"), Some("model_selection"));
        assert_eq!(match_dan_topic("prompt engineering"), Some("prompt_engineering"));
        // Case-insensitive.
        assert_eq!(match_dan_topic("PLANNING"), Some("planning"));
        // Ambiguous / unknown inputs resolve to None (caller falls back to
        // a free-text query, so /dan <anything> still finds content).
        assert_eq!(match_dan_topic("agent"), None); // agent_sandboxes + agents_learning + multi_agent
        assert_eq!(match_dan_topic("xyzzy"), None);
        assert_eq!(match_dan_topic(""), None);
        // The palette alias /d is documented via COMMANDS and handled.
        assert!(COMMANDS.iter().any(|(n, _)| *n == "dan"));
    }
}
