//! ByteAi TUI — jcode-style interaction, unique ByteAi layout.
//! Mirrors jcode's command surface (/help, /model, /clear, /save, /usage,
//! /session, /keys, /config, /subagent, /swarm, ...) and keybindings
//! (Ctrl+Shift+K/J scroll, Ctrl+Tab model switch, Alt+U/D page, Ctrl+C
//! interrupt), with a unique ByteAi header/banner/status layout.
//!
//! Responsiveness model (jcode-style): the agent turn runs in a background
//! tokio task; text tokens and tool events stream back through an mpsc
//! channel and are rendered live. While busy, the header shows a spinner
//! and an elapsed-seconds counter. Ctrl+C aborts the in-flight task.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apex_core::Agent;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};

const MAX_LOG: usize = 3000;
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// All supported /commands (name, description). Used by the command palette
/// that appears when the user types `/`.
const COMMANDS: &[(&str, &str)] = &[
    ("help", "show commands"),
    ("model", "show / switch model (e.g. /model name)"),
    ("models", "list models on provider"),
    ("provider", "list / switch provider (e.g. /provider name)"),
    ("addprovider", "add a new provider+model (e.g. /addprovider name url model key)"),
    ("tools", "list available tools"),
    ("clear", "clear conversation"),
    ("save", "save session to disk"),
    ("usage", "show token usage"),
    ("session", "list saved sessions"),
    ("config", "show config path"),
    ("keys", "show keybindings"),
    ("route", "route a task to the best model"),
    ("council", "multi-model deliberation vote"),
    ("govern", "constitutional guardrail check"),
    ("subagent", "spawn parallel subagents"),
    ("swarm", "spawn 3-way swarm"),
    ("quit", "exit"),
];

/// Messages streamed from the background agent task to the UI loop.
enum TurnMsg {
    Text(String),
    Tool(apex_types::ToolOutcome),
    Done(apex_types::AgentOutcome),
    Err(String),
}

#[derive(Clone)]
enum LogEntry {
    Text { content: String, style: Style },
    /// Streamed assistant text; chunks coalesce onto the current line so
    /// tokens don't wrap onto separate lines (jcode-style).
    Assistant(String),
    ToolCard { name: String, ok: bool, elapsed_ms: u64, output: String },
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
    /// Prompts queued while a turn is still running; auto-sent in order
    /// when the current turn finishes so questions are never dropped.
    pending_queue: Vec<String>,
    model: String,
    provider: String,
    tools_count: usize,
    last_tokens: u64,
    last_iters: u32,
    last_tools: u32,
    models: Vec<String>,
    model_idx: usize,
    turn_task: Option<tokio::task::JoinHandle<()>>,
    turn_rx: Option<tokio::sync::mpsc::UnboundedReceiver<TurnMsg>>,
    busy: bool,
    busy_since: Option<Instant>,
    spinner_frame: usize,
    /// Selected command palette index (0-based, 0 = first match).
    palette_idx: usize,
}

impl App {
    fn new(model: String, provider: String, tools_count: usize) -> Self {
        let mut app = Self {
            log: Vec::new(),
            input: String::new(),
            is_command: false,
            scroll: 0,
            model,
            provider,
            tools_count,
            last_tokens: 0,
            last_iters: 0,
            last_tools: 0,
            models: Vec::new(),
            model_idx: 0,
            turn_task: None,
            turn_rx: None,
            busy: false,
            busy_since: None,
            spinner_frame: 0,
            palette_idx: 0,
            follow_bottom: true,
            pending_queue: Vec::new(),
        };
        app.push_banner();
        app
    }

    fn push_banner(&mut self) {
        let banner = format!(
            "╔═ ByteAi APEX ═╗\n\
             ║ model: {} \n\
             ║ provider: {} \n\
             ║ tools: {} \n\
             ╚══════════════╝",
            self.model, self.provider, self.tools_count
        );
        for line in banner.lines() {
            self.log.push(LogEntry::Text {
                content: line.trim_end().to_string(),
                style: Style::default().fg(Color::Cyan),
            });
        }
        self.log.push(LogEntry::Meta(String::new()));
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
        let preview: String = output.chars().take(160).collect();
        let preview = if output.len() > 160 { format!("{preview}…") } else { preview };
        self.log.push(LogEntry::ToolCard {
            name: name.to_string(),
            ok,
            elapsed_ms,
            output: preview,
        });
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
        while self.log.len() > MAX_LOG {
            self.log.remove(0);
        }
    }

    fn add_error(&mut self, text: &str) {
        for line in text.lines() {
            self.log.push(LogEntry::Text {
                content: format!("⚠ {line}"),
                style: Style::default().fg(Color::Red),
            });
        }
        if self.follow_bottom {
            self.scroll = 0;
        }
    }

    fn add_meta(&mut self, text: impl Into<String>) {
        self.log.push(LogEntry::Meta(text.into()));
        if self.follow_bottom {
            self.scroll = 0;
        }
    }

    fn clear(&mut self) {
        self.log.clear();
        self.pending_queue.clear();
        self.push_banner();
        self.scroll = 0;
        self.follow_bottom = true;
    }

    /// Drain pending events from the background agent task into the log.
    fn drain(&mut self) {
        let mut rx = match self.turn_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        loop {
            match rx.try_recv() {
                Ok(TurnMsg::Text(t)) => self.add_stream(&t),
                Ok(TurnMsg::Tool(o)) => self.add_tool_card(&o.name, o.ok, o.elapsed_ms, &o.output),
                Ok(TurnMsg::Done(outcome)) => {
                    self.add_done(outcome.iterations, outcome.tool_calls_made, outcome.usage.total_tokens);
                    self.busy = false;
                    self.busy_since = None;
                    self.turn_task = None;
                    // Receiver consumed: leave turn_rx = None.
                    break;
                }
                Ok(TurnMsg::Err(e)) => {
                    self.add_error(&e);
                    self.busy = false;
                    self.busy_since = None;
                    self.turn_task = None;
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    // Keep draining next frame; put receiver back.
                    self.turn_rx = Some(rx);
                    return;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.turn_rx = None;
                    return;
                }
            }
        }
    }
}

pub async fn run() -> anyhow::Result<()> {
    let cfg = crate::config::load()?;
    let provider = crate::config::resolve_provider(&cfg, None, None, None);
    let model = crate::config::resolve_model(&cfg, None, &provider);
    let client = apex_provider::Client::new(provider.base_url.clone(), provider.resolved_key())?;
    let data_dir = crate::config::data_dir();
    let lsp = Arc::new(apex_lsp::LspRegistry::new(apex_lsp::default_servers()));
    let mut tool_ctx = apex_tools::ToolContext::with_lsp(data_dir.clone(), lsp);
    tool_ctx = tool_ctx.with_provider(client.clone(), model.clone());
    let tools = apex_tools::Registry::builtins(&tool_ctx);
    let tools_count = tools.names().len();
    let agent_cfg = apex_core::AgentConfig { model: model.clone(), ..Default::default() };
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(client, agent_cfg, tools, data_dir)));

    let mut app = App::new(model.clone(), provider.name.clone(), tools_count);
    // Warm the model list for Ctrl+Tab switching (best-effort, no block).
    if let Ok(ids) = agent.lock().await.provider.list_models().await {
        if let Some(pos) = ids.iter().position(|m| m == &model) {
            app.model_idx = pos;
        }
        app.models = ids;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
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
    loop {
        terminal.draw(|f| draw(f, app))?;
        app.drain();

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
        if let Event::Key(key) = ev {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // Ctrl+C: interrupt in-flight turn, else quit.
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                if app.busy {
                    if let Some(h) = app.turn_task.take() {
                        h.abort();
                    }
                    app.busy = false;
                    app.busy_since = None;
                    app.turn_rx = None;
                    app.pending_queue.clear();
                    app.add_meta("  ⚡ interrupted (Ctrl+C)");
                    continue;
                }
                break;
            }
            // Ctrl+Shift+K / Ctrl+Shift+J: scroll up/down (jcode).
            if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) && key.modifiers.contains(KeyModifiers::SHIFT) {
                app.follow_bottom = false;
                app.scroll = (app.scroll + 1).min(app.log.len().saturating_sub(1));
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
                app.scroll = (app.scroll + 20).min(app.log.len().saturating_sub(1));
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
                    app.add_meta(&format!("  model -> {}", app.model));
                } else {
                    app.add_meta("  no model list (provider offline)");
                }
                continue;
            }
            match key.code {
                KeyCode::Esc => {
                    app.input.clear();
                    app.is_command = false;
                    app.palette_idx = 0;
                }
                KeyCode::Enter => {
                    let text = app.input.trim().to_string();
                    app.input.clear();
                    app.is_command = false;
                    app.palette_idx = 0;
                    if text.is_empty() {
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
                            cmd_line = matches[app.palette_idx.min(matches.len() - 1)].to_string();
                        } else if matches.len() == 1 && matches[0] != cmd_line {
                            // Unique prefix -> complete to the full command.
                            cmd_line = matches[0].to_string();
                        } else if matches.len() > 1 && !matches.contains(&cmd_line.as_str()) {
                            // Ambiguous prefix -> run the highlighted match.
                            cmd_line = matches[app.palette_idx.min(matches.len() - 1)].to_string();
                        }
                        handle_command(agent, app, &cmd_line).await;
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
                        app.scroll = (app.scroll + 1).min(app.log.len().saturating_sub(1));
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
                    app.scroll = (app.scroll + 20).min(app.log.len().saturating_sub(1));
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
        let mut guard = a2.lock().await;
        let tx_text = tx.clone();
        let mut sink = move |t: &str| {
            let _ = tx_text.send(TurnMsg::Text(t.to_string()));
        };
        let tx_tool = tx.clone();
        let mut toolsink = move |o: &apex_types::ToolOutcome| {
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
}

async fn handle_command(agent: &Arc<tokio::sync::Mutex<Agent>>, app: &mut App, cmd_line: &str) {
    let parts: Vec<&str> = cmd_line.split_whitespace().collect();
    let cmd = parts.first().unwrap_or(&"");
    match *cmd {
        "help" | "h" | "?" | "commands" => {
            app.add_assistant("/help           — this message");
            app.add_assistant("/model <name>   — show / switch / persist model");
            app.add_assistant("/models         — list models on provider");
            app.add_assistant("/provider       — list / switch / persist provider");
            app.add_assistant("/addprovider    — add provider+model (e.g. /addprovider name url model)");
            app.add_assistant("/tools          — list available tools");
            app.add_assistant("/clear          — clear conversation");
            app.add_assistant("/save <name>    — save session to disk");
            app.add_assistant("/usage          — show token usage");
            app.add_assistant("/session        — list saved sessions");
            app.add_assistant("/config         — show config path");
            app.add_assistant("/keys           — show keybindings");
            app.add_assistant("/route          — route a task to the best model");
            app.add_assistant("/council        — multi-model deliberation vote");
            app.add_assistant("/govern         — constitutional guardrail check");
            app.add_assistant("/subagent       — spawn parallel subagents");
            app.add_assistant("/swarm          — spawn 3-way swarm");
            app.add_assistant("/quit           — exit");
        }
        "model" => {
            if let Some(m) = parts.get(1) {
                let m = m.to_string();
                // Persist so the choice survives restart (agent + provider).
                let mut cfg = crate::config::load().unwrap_or_default();
                if let Err(e) = crate::config::set_model(&mut cfg, &app.provider, &m) {
                    app.add_error(&format!("  could not persist model: {e:#}"));
                }
                agent.lock().await.config.model = m.clone();
                app.model = m.clone();
                app.add_meta(format!("  model -> {m}"));
            } else {
                app.add_meta(format!("  model = {}", agent.lock().await.config.model));
                // Show models available on the current provider (best-effort).
                match agent.lock().await.provider.list_models().await {
                    Ok(ids) => {
                        app.models = ids.clone();
                        app.add_meta(format!(
                            "  {} models on {}: {}",
                            ids.len(),
                            app.provider,
                            ids.join(", ")
                        ));
                    }
                    Err(_) => app.add_meta("  (provider model list offline)"),
                }
            }
        }
        "models" => {
            match agent.lock().await.provider.list_models().await {
                Ok(ids) => {
                    app.models = ids.clone();
                    app.add_meta(format!("  {} models: {}", ids.len(), ids.join(", ")));
                }
                Err(e) => app.add_error(&format!("list_models failed: {e:#}")),
            }
        }
        "provider" => {
            let cfg = crate::config::load().unwrap_or_default();
            match parts.get(1).copied() {
                None => {
                    app.add_meta(format!("  provider = {}", app.provider));
                    app.add_meta("  configured providers:");
                    for p in &cfg.providers {
                        let cur = if p.name == app.provider { " ▸" } else { "  " };
                        let key = if p.resolved_key().is_empty() { " (no key)" } else { "" };
                        app.add_meta(format!("  {cur} {}{key}", p.name));
                    }
                    app.add_meta("  switch: /provider <name> · add: /addprovider <name> <url> <model> [key]");
                }
                Some(name) => {
                    // Switch provider at runtime: rebuild the client and model.
                    let provider = crate::config::resolve_provider(&cfg, Some(name), None, None);
                    if provider.name != name {
                        app.add_error(&format!("  provider '{name}' not found — see /provider"));
                        return;
                    }
                    match apex_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
                        Ok(client) => {
                            // Persist as default provider.
                            let mut cfg2 = crate::config::load().unwrap_or_default();
                            let _ = crate::config::set_default_provider(&mut cfg2, name);
                            let model = crate::config::resolve_model(&cfg, None, &provider);
                            {
                                let mut g = agent.lock().await;
                                g.provider = client;
                                g.config.model = model.clone();
                            }
                            app.provider = name.to_string();
                            app.model = model.clone();
                            // Refresh the model list for the new provider.
                            if let Ok(ids) = agent.lock().await.provider.list_models().await {
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
                    match apex_provider::Client::new(url.to_string(), key_val) {
                        Ok(client) => {
                            let m = if model.is_empty() { app.model.clone() } else { model.to_string() };
                            {
                                let mut g = agent.lock().await;
                                g.provider = client;
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
            let names = agent.lock().await.tools.names();
            app.add_meta(format!("  tools ({}) {}", names.len(), names.join(", ")));
        }
        "clear" | "new" => {
            app.clear();
        }
        "save" => {
            let name = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| {
                format!("tui-{}", crate::session::new_id().get(..8).unwrap_or("sess"))
            });
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
            match crate::session::list() {
                Ok(list) if list.is_empty() => app.add_meta("  no saved sessions"),
                Ok(list) => {
                    for s in list.iter().take(10) {
                        let msgs = s.messages.len();
                        let last = s.messages.last().and_then(|m| m.content.clone()).unwrap_or_default();
                        let preview: String = last.chars().take(50).collect();
                        app.add_meta(format!("  {} · {msgs} msgs · {preview}", s.id));
                    }
                    if list.len() > 10 {
                        app.add_meta(format!("  ... {} more", list.len() - 10));
                    }
                }
                Err(e) => app.add_error(&format!("session list failed: {e:#}")),
            }
        }
        "usage" => {
            app.add_meta(format!("  tokens: {} total", agent.lock().await.usage.total_tokens));
        }
        "config" => {
            app.add_meta(format!("  config: {}", crate::config::config_dir().join("config.toml").display()));
            app.add_meta(format!("  data:   {}", crate::config::data_dir().display()));
        }
        "keys" | "keybindings" => {
            app.add_assistant("/keys — jcode-style keybindings");
            app.add_assistant("  Ctrl+Shift+K/J   scroll up/down");
            app.add_assistant("  Alt+U / Alt+D    page up/down");
            app.add_assistant("  Ctrl+Tab         next model  ·  Ctrl+Shift+Tab  prev model");
            app.add_assistant("  Up / Down        scroll transcript");
            app.add_assistant("  Esc              clear input");
            app.add_assistant("  Tab              toggle command mode");
            app.add_assistant("  Ctrl+C           interrupt turn / quit");
        }
        "subagent" | "swarm" => {
            let count = if *cmd == "swarm" { 3 } else { 1 };
            let goal = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| "Review the current workspace".to_string());
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
                app.add_error("usage: /council <question>");
            } else {
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
        }
        "govern" => {
            let action = parts.get(1..).unwrap_or(&[]).join(" ");
            if action.is_empty() {
                app.add_error("usage: /govern <action to check>");
            } else {
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
    if palette_h > 0 {
        draw_palette(f, chunks[2], app);
    }
    draw_input(f, chunks[3], app);
    draw_status(f, chunks[4], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &mut App) {
    let busy = if app.busy {
        let sp = SPINNER[(app.spinner_frame / 2) % SPINNER.len()];
        let secs = app.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        format!(" {sp} {secs}s")
    } else {
        String::new()
    };
    let header = Line::from(vec![
        Span::styled(" ByteAi ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled("APEX", Style::default().fg(Color::Magenta)),
        Span::raw(" · "),
        Span::styled(&app.model, Style::default().fg(Color::White)),
        Span::raw(" · "),
        Span::styled(&app.provider, Style::default().fg(Color::Yellow)),
        Span::raw(" · "),
        Span::styled(format!("{} tools", app.tools_count), Style::default().fg(Color::Gray)),
        Span::styled(busy, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(header).style(Style::default().bg(Color::Black)), area);
}

/// Estimated terminal rows an entry consumes after wrapping at `width`.
fn entry_rows(e: &LogEntry, width: usize) -> usize {
    let w = width.max(1);
    let lines = |s: &str| -> usize { s.lines().map(|l| l.chars().count().div_ceil(w).max(1)).sum::<usize>().max(1) };
    match e {
        LogEntry::Text { content, .. } => lines(content),
        LogEntry::Assistant(content) => lines(content),
        LogEntry::ToolCard { output, ok, .. } => {
            let mut r = 1;
            if !output.is_empty() && *ok {
                r += lines(output);
            }
            r
        }
        LogEntry::Meta(text) => lines(text),
    }
}

fn draw_chat(f: &mut Frame, area: Rect, app: &App) {
    let max_rows = area.height as usize;
    let total = app.log.len();
    let busy_rows = if app.busy { 1 } else { 0 };

    // Build ALL log lines as styled Paragraph lines. Paragraph wraps long
    // lines at the width (List truncates them — that was the clipping bug),
    // so every word of an answer stays on screen.
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.log {
        match entry {
            LogEntry::Text { content, style } => {
                lines.push(Line::from(Span::styled(content.clone(), *style)));
            }
            LogEntry::Assistant(content) => {
                lines.push(Line::from(Span::styled(
                    content.clone(),
                    Style::default().fg(Color::White),
                )));
            }
            LogEntry::ToolCard { name, ok, elapsed_ms, output } => {
                let icon = if *ok { "✓" } else { "✗" };
                let color = if *ok { Color::Yellow } else { Color::Red };
                lines.push(Line::from(vec![
                    Span::styled("  [tool] ", Style::default().fg(Color::DarkGray)),
                    Span::styled(name.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Span::raw(" "),
                    Span::styled(icon, Style::default().fg(color)),
                    Span::raw(" "),
                    Span::styled(format!("({elapsed_ms} ms)"), Style::default().fg(Color::DarkGray)),
                ]));
                if !output.is_empty() && *ok {
                    lines.push(Line::from(Span::styled(
                        format!("         {output}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            LogEntry::Meta(text) => {
                lines.push(Line::from(Span::styled(
                    text.clone(),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    // Spinner + elapsed seconds appear right in the chat while the agent is
    // working, so the user sees activity at the answer point (jcode-style).
    if app.busy {
        let sp = SPINNER[(app.spinner_frame / 2) % SPINNER.len()];
        let secs = app.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled(format!(" {sp} "), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{secs}s working…"), Style::default().fg(Color::Yellow)),
        ]));
    }

    // Estimate total wrapped rows to pin the scroll to the newest content.
    let mut total_rows: usize = lines.len().min(total);
    // (Paragraph wraps each line; estimate per original entry for accuracy.)
    total_rows = app
        .log
        .iter()
        .map(|e| entry_rows(e, area.width as usize))
        .sum::<usize>()
        + busy_rows;

    // Scroll offset (rows skipped from the top). follow_bottom pins to the
    // newest row; manual scroll goes `app.scroll` lines up from the bottom.
    let at_bottom = total_rows.saturating_sub(max_rows);
    let offset = if app.follow_bottom {
        at_bottom
    } else {
        at_bottom.saturating_add(app.scroll)
    };

    let paragraph = Paragraph::new(lines)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll(((offset.min(total_rows)) as u16, 0));
    f.render_widget(paragraph, area);
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

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let prefix = if app.is_command { " / " } else { "> " };
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
        .block(Block::default().borders(Borders::ALL).style(Style::default()))
        .scroll((scroll as u16, 0))
        .style(Style::default().fg(Color::White));
    f.render_widget(input, area);

    // Cursor at the end of input, on the last visible wrapped row.
    let len = chars.len();
    let col = len % inner_w;
    let cur_line = (len / inner_w).saturating_sub(scroll);
    let cursor_x = area.x + 1 + col as u16;
    let cursor_y = area.y + 1 + cur_line as u16;
    f.set_cursor(cursor_x, cursor_y);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let status = format!(
        "  model: {} | provider: {} | tokens: {} | ·{} iter {} tools | Ctrl+Shift+K/J scroll | Ctrl+C quit",
        app.model, app.provider, app.last_tokens, app.last_iters, app.last_tools
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

    /// Every command in the palette must be handled by handle_command.
    #[test]
    fn every_palette_command_is_handled() {
        let handled = [
            "help", "model", "models", "provider", "addprovider", "tools",
            "clear", "save", "usage", "session", "config", "keys", "subagent",
            "swarm", "route", "council", "govern", "quit",
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
    fn command_names_are_unique() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        for w in names.windows(2) {
            assert_ne!(w[0], w[1], "duplicate command /{}", w[0]);
        }
    }

    #[test]
    fn matching_commands_prefixes() {
        // Empty prefix matches everything.
        assert_eq!(matching_commands("").len(), COMMANDS.len());
        // Unique prefix.
        assert_eq!(matching_commands("prov"), vec!["provider"]);
        assert_eq!(matching_commands("hel"), vec!["help"]);
        // Ambiguous prefix (model, models).
        assert_eq!(matching_commands("mo"), vec!["model", "models"]);
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
}
