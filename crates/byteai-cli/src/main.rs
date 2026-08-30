//! ByteAi — Phase 1 CLI.
//!
//! Subcommands: chat (one-shot or REPL), session (save/load/list), doctor,
//! models, tui. Provider-agnostic over any OpenAI-compatible endpoint.

mod config;
mod session;
mod serve;
mod setup;
#[cfg(feature = "tui")]
mod tui;
mod toolcards;

use std::sync::Arc;

use anyhow::{Context, Result};
use byteai_core::{Agent, AgentConfig, SYSTEM_PROMPT};
use byteai_provider::Client;
use byteai_tools::{Registry, Tool, ToolContext};
use byteai_types::Message;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(name = "byteai", version, about = "ByteAi — fast autonomous coding agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// One-shot or interactive chat.
    Chat(ChatArgs),
    /// Session management.
    Session(SessionArgs),
    /// Check provider connectivity and configuration.
    Doctor,
    /// List models available on the resolved provider.
    Models,
    /// Launch the terminal UI.
    Tui,
    /// Invoke a built-in tool directly (no LLM). For debugging/verification.
    #[command(name = "tool")]
    Tool(ToolArgs),
    /// Run an HTTP daemon: proxy chat completions to the provider (router mode)
    /// and expose built-in tools over HTTP (daemon mode).
    Serve(ServeArgs),
    /// Interactive first-run wizard: providers, models, skills, tools, agent settings.
    Setup,
    /// GitHub integration: connect/status/push via gh CLI, or discovery actions.
    Github(GithubArgs),
    /// TencentDB Agent Memory management: status, setup, search, capture.
    Memory(MemoryArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Port to listen on (default 8424).
    #[arg(long, default_value_t = 8424)]
    port: u16,
    /// Override provider base URL (router mode target).
    #[arg(long)]
    base_url: Option<String>,
    /// Override API key.
    #[arg(long)]
    api_key: Option<String>,
}

#[derive(clap::Args)]
struct ToolArgs {
    /// Tool name (e.g. lsp, read, edit, search).
    name: String,
    /// JSON arguments for the tool.
    args: String,
}

#[derive(clap::Args, Default)]
struct ChatArgs {
    /// Prompt (one-shot). Omit for interactive REPL.
    prompt: Option<String>,
    /// Provider name from config (e.g. omniroute, bai).
    #[arg(long)]
    provider: Option<String>,
    /// Override base URL.
    #[arg(long)]
    base_url: Option<String>,
    /// Override API key.
    #[arg(long)]
    api_key: Option<String>,
    /// Override model.
    #[arg(long)]
    model: Option<String>,
    /// Disable tool calling.
    #[arg(long)]
    no_tools: bool,
    /// Resume a saved session by id.
    #[arg(long)]
    resume: Option<String>,
    /// Auto-save session with this name.
    #[arg(long)]
    save: Option<String>,
    /// Override the per-turn iteration cap (0 = unlimited). Used by the
    /// spawn/delegation tool to give each child its own independent budget.
    #[arg(long)]
    max_iterations: Option<u32>,
    /// Override the wall-clock run budget in seconds (0 = off). Whichever of
    /// iteration cap / this budget hits first wraps the turn up gracefully.
    #[arg(long)]
    budget_seconds: Option<u64>,
    /// Force full autonomy (CAP ON): byteai never pauses to ask the user
    /// questions — it decides autonomously and keeps working. When OFF (default
    /// from config), a model question that looks like it's asking the user
    /// pauses the turn. Use --cap for sub-agents and background tasks that
    /// cannot receive stdin.
    #[arg(long, default_value_t = false)]
    cap: bool,
}

#[derive(clap::Args)]
struct SessionArgs {
    #[command(subcommand)]
    cmd: SessionCmd,
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Save the current conversation (requires --save on chat).
    Save { name: String },
    /// Load a session and continue interactively.
    Load { id: String },
    /// List saved sessions.
    List,
    /// Capture a session's messages into the memory store (claude-mem pattern).
    Capture { name: Option<String> },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // The TUI installs its own tracing pipeline (WARN/ERROR events are routed
    // into the chat log as notifications instead of stderr), so logs can never
    // land at the terminal cursor and corrupt the typing box. Every other
    // surface keeps the plain stderr formatter.
    let is_tui = cfg!(feature = "tui")
        && (cli.cmd.is_none() || matches!(cli.cmd, Some(Cmd::Tui)));
    if !is_tui {
        init_stderr_tracing();
    }

    match cli.cmd {
        None => {
            // Bare `byteai` → TUI (the friendly default surface, oh-my-pi style).
            #[cfg(feature = "tui")]
            {
                tui::run().await.context("TUI failed")
            }
            #[cfg(not(feature = "tui"))]
            {
                // No TUI compiled in — fall back to the REPL.
                let args = ChatArgs {
                    prompt: None,
                    provider: None,
                    base_url: None,
                    api_key: None,
                    model: None,
                    no_tools: false,
                    resume: None,
                    save: None,
                    max_iterations: None,
                    budget_seconds: None,
                    cap: false,
                };
                chat(args).await
            }
        }
        Some(Cmd::Doctor) => doctor().await,
        Some(Cmd::Models) => models().await,
        Some(Cmd::Chat(args)) => chat(args).await,
        Some(Cmd::Session(sa)) => session_cmd(sa).await,
        Some(Cmd::Tui) => {
            #[cfg(feature = "tui")]
            {
                tui::run().await.context("TUI failed")
            }
            #[cfg(not(feature = "tui"))]
            {
                anyhow::bail!("TUI not compiled in; rebuild with --features tui")
            }
        }
        Some(Cmd::Tool(ta)) => tool_cmd(ta).await,
        Some(Cmd::Serve(sa)) => serve_cmd(sa).await,
        Some(Cmd::Setup) => setup::run(),
        Some(Cmd::Github(args)) => github_cmd(args).await,
        Some(Cmd::Memory(args)) => memory_cmd(args).await,
    }
}

/// Plain stderr tracing formatter used by non-TUI surfaces (chat, repl,
/// serve, tool, doctor, models, session). The TUI must NOT use this: stderr
/// writes land at the terminal cursor in raw mode and corrupt the input box.
fn init_stderr_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,byteai=info".into()),
        )
        .with_target(false)
        .init();
}

/// `byteai serve` — HTTP daemon (router + remote tool invocations).
async fn serve_cmd(sa: crate::ServeArgs) -> Result<()> {
    let data_dir = config::data_dir();
    let lsp = Arc::new(byteai_lsp::LspRegistry::new(byteai_lsp::default_servers()));
    let dap = Arc::new(byteai_dap::DapRegistry::new(byteai_dap::default_adapters()));
    let mut ctx = ToolContext::with_all(data_dir, lsp, dap);
    if let Ok(cfg) = config::load() {
        let provider = config::resolve_provider(&cfg, None, sa.base_url.as_deref(), sa.api_key.as_deref());
        let model = config::resolve_model(&cfg, None, &provider);
        if let Ok(client) = Client::new(provider.base_url.clone(), provider.resolved_key()) {
            ctx = ctx.with_provider(client, model);
        }
    }
    let tools = Registry::builtins(&ctx);
    serve::run(
        serve::ServeArgs {
            port: sa.port,
            base_url: sa.base_url,
            api_key: sa.api_key,
        },
        ctx,
        tools,
    )
    .await
}

/// Direct tool invocation (no LLM): `byteai tool <name> '<json args>'`.
async fn tool_cmd(ta: ToolArgs) -> Result<()> {
    let data_dir = config::data_dir();
    let lsp = Arc::new(byteai_lsp::LspRegistry::new(byteai_lsp::default_servers()));
    let dap = Arc::new(byteai_dap::DapRegistry::new(byteai_dap::default_adapters()));
    let mut ctx = ToolContext::with_all(data_dir, lsp, dap);
    // Attach provider client (best-effort) for route/council/govern tools.
    if let Ok(cfg) = config::load() {
        let provider = config::resolve_provider(&cfg, None, None, None);
        let model = config::resolve_model(&cfg, None, &provider);
        if let Ok(client) = Client::new(provider.base_url.clone(), provider.resolved_key()) {
            ctx = ctx.with_provider(client, model);
        }
    }
    let tools = Registry::builtins(&ctx);
    let args: serde_json::Value = serde_json::from_str(&ta.args)
        .with_context(|| format!("invalid JSON args: {}", ta.args))?;
    let tool = tools
        .get(&ta.name)
        .with_context(|| format!("unknown tool {:?}; available: {}", ta.name, tools.names().join(", ")))?;
    let outcome = tool.execute(args).await;
    println!("{}", outcome.output);
    if !outcome.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// Build the agent from CLI/config resolution.
///
/// Constructs a ProviderPool from ALL configured providers that have a
/// resolved key (or the explicitly-selected provider), so the agent can
/// fail over to a second provider if the primary dies mid-task.
fn build_agent(
    args: &ChatArgs,
    no_tools: bool,
) -> Result<(Agent, String, String)> {
    let cfg = config::load()?;
    let provider = config::resolve_provider(&cfg, args.provider.as_deref(), args.base_url.as_deref(), args.api_key.as_deref());
    let model = config::resolve_model(&cfg, args.model.as_deref(), &provider);
    let agent_cfg = AgentConfig {
        model: model.clone(),
        max_iterations: args.max_iterations.unwrap_or(cfg.agent.max_iterations),
        run_budget_seconds: match args.budget_seconds {
            Some(0) | None => cfg.agent.run_budget_seconds.filter(|&b| b > 0),
            Some(b) => Some(b),
        },
        warn_ratio: cfg.agent.budget_warn_ratio,
        tool_timeout: std::time::Duration::from_secs(cfg.agent.tool_timeout_seconds.unwrap_or(300)),
        tools_enabled: !no_tools,
        auto_continue: cfg.agent.auto_continue,
        cap_enabled: cfg.agent.cap_enabled || args.cap,
        tool_select: cfg.agent.tool_select && !no_tools,
        tool_select_max: cfg.agent.tool_select_max,
        tdai: cfg.memory.to_tdai_config(),
        ..AgentConfig::default()
    };
    let data_dir = config::data_dir();
    let lsp = Arc::new(byteai_lsp::LspRegistry::new(byteai_lsp::default_servers()));
    let dap = Arc::new(byteai_dap::DapRegistry::new(byteai_dap::default_adapters()));
    let mut ctx = ToolContext::with_all(data_dir.clone(), lsp, dap);
    if let Ok(client) = byteai_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
        ctx = ctx.with_provider(client, model.clone());
    }
    let tools = Registry::builtins(&ctx);

    // Build a failover pool: the resolved provider first, then every other
    // configured provider with a resolved key (deduped by name). The pool
    // starts on the resolved provider; on hard failure the agent rotates.
    // When --provider is explicitly passed, ONLY that provider is included
    // (to avoid auth issues from providers with mismatched keys).
    let mut entries: Vec<byteai_provider::pool::ProviderEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |name: String, p: &config::ProviderEntry, mdl: String| {
        if seen.contains(&name) {
            return;
        }
        if let Ok(c) = Client::new(p.base_url.clone(), p.resolved_key()) {
            entries.push(byteai_provider::pool::ProviderEntry { name: name.clone(), client: c, model: mdl });
            seen.insert(name);
        }
    };
    push(provider.name.clone(), &provider, model.clone());
    if args.provider.is_none() {
        for p in &cfg.providers {
            if p.name == provider.name {
                continue;
            }
            // Prefer the provider's own model, else the effective model.
            let mdl = if p.model.is_empty() { model.clone() } else { p.model.clone() };
            push(p.name.clone(), p, mdl);
        }
    }
    let pool = byteai_provider::pool::ProviderPool::new(entries);

    let agent = Agent::new(pool, agent_cfg, tools, data_dir.clone());
    Ok((agent, provider.name.clone(), model))
}

async fn chat(mut args: ChatArgs) -> Result<()> {
    let (mut agent, provider_name, model) = build_agent(&args, args.no_tools)?;

    // Resume: load messages into the agent.
    if let Some(id) = args.resume.take() {
        let sf = session::load(&id)?;
        agent.history = sf.messages;
        agent.usage = sf.usage;
        agent.with_system_prompt();
        println!("[resumed session {id}: {} messages, {} tokens]", agent.history.len(), agent.usage.total_tokens);
    }

    match args.prompt.take() {
        Some(prompt) => {
            // Fire due scheduled jobs before the one-shot turn.
            run_due_jobs();
            // Loop on needs_input: when the agent asks a question (CAP off),
            // WAIT for the user's answer on the terminal instead of letting
            // the agent auto-answer. Each answer continues the same turn.
            let mut p = prompt;
            loop {
                let (outcome, turn_tools, turn_ms) = run_turn(&mut agent, &p).await?;
                if outcome.needs_input {
                    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                        println!();
                        println!("[type your answer and press Enter to continue the task — or Enter alone to stop]");
                        use std::io::BufRead;
                        let mut line = String::new();
                        let read = std::io::stdin().lock().read_line(&mut line)?;
                        if read == 0 {
                            report(&outcome, &agent, &turn_tools, turn_ms);
                            break;
                        }
                        let answer = line.trim().to_string();
                        if answer.is_empty() {
                            report(&outcome, &agent, &turn_tools, turn_ms);
                            break;
                        }
                        p = answer;
                        continue;
                    }
                    // Non-interactive (piped) stdin: don't hang. Print the
                    // question + paused state; the user resumes interactively.
                    println!();
                    println!("[✋ byteai asked you a question and is waiting for your answer. Run byteai interactively to continue the same task.]");
                    report(&outcome, &agent, &turn_tools, turn_ms);
                    break;
                }
                report(&outcome, &agent, &turn_tools, turn_ms);
                break;
            }
        }
        None => {
            repl(&mut agent).await?;
        }
    }

    if let Some(name) = args.save.take() {
        let mut sf = session::from_agent(&model, &provider_name, agent.history.clone(), agent.usage.clone());
        sf.id = name;
        let path = session::save(&sf)?;
        println!("[session saved: {}]", path.display());
    }
    Ok(())
}

async fn session_cmd(sa: SessionArgs) -> Result<()> {
    match sa.cmd {
        SessionCmd::List => {
            let sessions = session::list()?;
            if sessions.is_empty() {
                println!("No saved sessions.");
            }
            for s in sessions {
                let msgs = s.messages.len();
                let last = s.messages.last().and_then(|m| m.content.clone()).unwrap_or_default();
                let preview: String = last.chars().take(60).collect();
                println!("{:36} {} msgs {:>8} tok  {}{}", s.id, msgs, s.usage.total_tokens, s.provider, if preview.is_empty() { String::new() } else { format!(" | {preview}…") });
            }
            Ok(())
        }
        SessionCmd::Load { id } => {
            let sf = session::load(&id)?;
            println!("[loaded {id}: {} messages]", sf.messages.len());
            // Show tail for context.
            for m in sf.messages.iter().rev().take(4).rev() {
                if let Some(c) = &m.content {
                    let p: String = c.chars().take(120).collect();
                    println!("{:>9}| {}", format!("{:?}", m.role).to_lowercase(), p);
                }
            }
            Ok(())
        }
        SessionCmd::Save { name } => {
            anyhow::bail!("`session save` requires an active conversation; use `byteai chat --save {name}`")
        }
        SessionCmd::Capture { name } => {
            // claude-mem pattern: archive a session into the memory store.
            let id = name.unwrap_or_else(|| {
                session::list().ok().and_then(|s| s.first().map(|s| s.id.clone())).unwrap_or_default()
            });
            let sf = session::load(&id)?;
            let mut out = String::new();
            match byteai_memory::Memory::open(&config::data_dir().join("memory")) {
                Ok(mut mem) => {
                    let mut count = 0u32;
                    for m in &sf.messages {
                        if let Some(content) = &m.content {
                            if let Err(e) = mem.log_session_message(&id, &format!("{:?}", m.role), content) {
                                out.push_str(&format!("  log error: {e:#}\n"));
                            } else {
                                count += 1;
                            }
                        }
                    }
                    out.push_str(&format!("captured {count} messages from session {id} into memory"));
                }
                Err(e) => out.push_str(&format!("memory unavailable: {e:#}")),
            }
            println!("{out}");
            Ok(())
        }
    }
}

async fn doctor() -> Result<()> {
    let cfg = config::load()?;
    println!("ByteAi doctor — config: {}", config::config_dir().join("config.toml").display());
    println!();
    let mut any_ok = false;
    for p in &cfg.providers {
        let key = p.resolved_key();
        let has_key = !key.is_empty();
        let client = match Client::new(p.base_url.clone(), key) {
            Ok(c) => c,
            Err(e) => {
                println!("[{}] build client failed: {e:#}", p.name);
                continue;
            }
        };
        match client.list_models().await {
            Ok(ids) => {
                any_ok = true;
                println!("[{}] OK  {} models  (base_url={})", p.name, ids.len(), p.base_url);
                if !has_key {
                    println!("      (no API key resolved — unauthenticated endpoint)");
                }
                let smol: Vec<&str> = ids.iter().filter(|i| i.starts_with("auto/")).take(8).map(|s| s.as_str()).collect();
                if !smol.is_empty() {
                    println!("      role aliases: {}", smol.join(", "));
                }
            }
            Err(e) => {
                println!("[{}] FAIL {:#}", p.name, e);
            }
        }
    }
    // LSP servers on PATH.
    let lsp = byteai_lsp::LspRegistry::new(byteai_lsp::default_servers());
    let langs = lsp.available();
    println!();
    println!("LSP servers on PATH: {}", if langs.is_empty() { "none".into() } else { langs.join(", ") });
    for spec in byteai_lsp::default_servers() {
        let on_path = byteai_lsp::command_on_path(&spec.command);
        println!("  {:<10} {} {}", spec.lang, if on_path { "OK " } else { "MISSING" }, spec.command);
    }
    // DAP adapter availability check (shared with the DAP registry).
    println!();
    println!("DAP adapters on PATH:");
    for spec in byteai_dap::default_adapters() {
        let on_path = byteai_dap::command_on_path(&spec.command);
        println!("  {:<10} {} {}{}", spec.lang, if on_path { "OK " } else { "MISSING" }, spec.command, if on_path && spec.adapter_id != "debugpy" { " (stdio)" } else { " (adapter)" });
    }
    // Memory + skills.
    match byteai_memory::Memory::open(&config::data_dir().join("memory")) {
        Ok(mem) => {
            if let Ok(stats) = mem.stats() {
                println!();
                println!("Memory (SQLite+FTS5): {}", stats.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "));
            }
        }
        Err(e) => println!("\nMemory: unavailable ({e:#})"),
    }
    if !any_ok {
        println!();
        println!("No reachable provider. Set BYTEAI_BASE_URL / BYTEAI_API_KEY / BYTEAI_MODEL");
        println!("or create {} with a [[providers]] entry.", config::config_dir().join("config.toml").display());
        std::process::exit(1);
    }
    Ok(())
}

async fn models() -> Result<()> {
    let cfg = config::load()?;
    let provider = config::resolve_provider(&cfg, None, None, None);
    let client = Client::new(provider.base_url.clone(), provider.resolved_key())?;
    let ids = client.list_models().await?;
    for id in ids {
        println!("{id}");
    }
    Ok(())
}

async fn run_turn(agent: &mut Agent, prompt: &str) -> Result<(byteai_types::AgentOutcome, Vec<(String, u64)>, u64)> {
    let start = std::time::Instant::now();
    let live = agent.live.clone();
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel::<()>(1);
    let poller = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if let Ok(l) = live.try_lock()
                        && (l.iterations > 0 || !l.active_tools.is_empty()) {
                            eprint!("\r  {} \x1b[K", l.line(0));
                        }
                }
            }
        }
        eprint!("\r\x1b[K");
    });
    let mut text_sink = |t: &str| print!("{t}");
    let ansi = toolcards::stdout_is_tty();
    let mut turn_tools: Vec<(String, u64)> = Vec::new();
    let mut tool_sink = |o: &byteai_types::ToolOutcome| {
        turn_tools.push((o.name.clone(), o.elapsed_ms));
        let box_w = toolcards::term_width();
        println!();
        println!("{}", toolcards::cli_card(&o.name, o.ok, o.elapsed_ms, &o.output, box_w, ansi));
    };
    let outcome = agent.run(prompt, &mut text_sink, &mut tool_sink).await?;
    let _ = stop_tx.send(()).await;
    let _ = poller.await;
    let turn_ms = start.elapsed().as_millis() as u64;
    println!();
    Ok((outcome, turn_tools, turn_ms))
}

fn report(outcome: &byteai_types::AgentOutcome, agent: &Agent, turn_tools: &[(String, u64)], turn_ms: u64) {
    println!();
    if outcome.needs_input {
        // The model asked the user a question; the turn paused. The question
        // itself was already streamed. Tell the user we're waiting and how
        // to continue (their next input continues the same task).
        println!("[✋ awaiting your input — byteai asked you a question. Type your answer and press Enter to continue the same task.]");
        return;
    }
    if !turn_tools.is_empty() {
        println!("{}", toolcards::ribbon(turn_tools, turn_ms));
    }
    if outcome.finished {
        println!("[done: {} iterations, {} tool calls, {} tokens, phase={}]", outcome.iterations, outcome.tool_calls_made, outcome.usage.total_tokens, agent.phase.as_str());
        if outcome.exhausted {
            let reason = outcome.exhausted_reason.as_deref().unwrap_or("interaction budget");
            println!("[⚠ {reason} reached — final answer from partial progress]");
        }
    } else {
        println!("[blocked: {} — {}]", outcome.blocked_reason.as_deref().unwrap_or("unknown"), agent.phase.as_str());
    }
}

/// Fire any due scheduled jobs (Hermes cron parity). Call this at the start
/// of each REPL iteration and before one-shot turns so background jobs
/// actually run — the schedule tool persists jobs, this worker executes them.
fn run_due_jobs() -> usize {
    let data_dir = config::data_dir();
    let tool = byteai_tools::schedule::ScheduleTool::new(data_dir.clone());
    // Normalize: the tool's constructor takes the data dir path, same as
    // the registry uses when constructing the tool (data_dir.join("schedule.json")).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let fired = tool.tick(now);
    for j in &fired {
        println!(
            "[schedule] {} fired — runs={} result={}",
            j.name,
            j.runs,
            j.last_result.chars().take(80).collect::<String>()
        );
    }
    fired.len()
}

/// Interactive REPL with a small command surface.
async fn repl(agent: &mut Agent) -> Result<()> {
    println!("ByteAi REPL — model {} · {} tools. Type /help for commands.", agent.config.model, agent.tools.names().len());
    println!("(multiline: end a line with \\ )");
    let stdin = std::io::stdin();
    let mut buffer = String::new();
    loop {
        // Fire due scheduled jobs before each prompt so background work runs
        // even while the user is typing.
        run_due_jobs();
        let mut line = String::new();
        use std::io::BufRead;
        let read = stdin.lock().read_line(&mut line)?;
        if read == 0 {
            println!();
            break; // EOF
        }
        let line = line.trim_end();
        if let Some(stripped) = line.strip_suffix('\\') {
            buffer.push_str(stripped);
            buffer.push('\n');
            continue;
        }
        let full = format!("{buffer}{line}");
        buffer.clear();
        let full = full.trim().to_string();
        if full.is_empty() {
            continue;
        }
        if let Some(cmd) = full.strip_prefix('/') {
            match cmd.split_whitespace().next().unwrap_or("") {
                "help" | "h" => {
                    println!("/help           — this message");
                    println!("/model <name>   — show or switch model (e.g. /model deepseek-v4-flash)");
                    println!("/provider <name> — show or switch provider (e.g. /provider omniroute)");
                    println!("/tools          — list available tools");
                    println!("/route <type> <task> — route a task to the best model");
                    println!("/council <question>  — multi-model deliberation vote");
                    println!("/govern <action>    — constitutional guardrail check");
                    println!("/ideas [focus]  — discover top product ideas from real problems");
                    println!("                  (/ideas menu · /ideas research <idea> · /ideas build <idea>)");
                    println!("/github [target query] — discover+score skills/tools/harnesses/mcp");
                    println!("                  (/github skills <cap> · /github improve · /github current)");
                    println!("/github connect [repo] [public|private] — publish this project to GitHub (gh CLI)");
                    println!("/github status — GitHub auth + repo status · /github push — push latest");
                    println!("/save <name>    — save this session");
                    println!("/usage          — show token usage");
                    println!("/cap            — toggle CAP (Coding Auto-Pilot): ON=autonomous, OFF=wait for your answers");
                    println!("/setup          — interactive setup wizard (providers, models, skills, tools)");
                    println!("/clear          — clear conversation history");
                    println!("/settings       — show current settings");
                    println!("/quit           — exit");
                }
                "model" => {
                    if let Some(m) = cmd.split_whitespace().nth(1) {
                        agent.config.model = m.to_string();
                        // Persist so the choice survives restart.
                        let mut cfg = config::load().unwrap_or_default();
                        let prov = cfg.agent.default_provider.clone();
                        let _ = config::set_model(&mut cfg, &prov, m);
                        println!("[model -> {m}]");
                    } else {
                        println!("[model = {}]", agent.config.model);
                    }
                }
                "provider" => {
                    let cfg = config::load().unwrap_or_default();
                    match cmd.split_whitespace().nth(1) {
                        Some("add") => {
                            // /provider add <name> <base_url> <model> [key|env:KEY]
                            let parts: Vec<&str> = cmd.split_whitespace().collect();
                            if parts.len() < 4 {
                                println!("[usage: /provider add <name> <base_url> <model> [key|env:KEY]]");
                                continue;
                            }
                            let (name, url, model) = (parts[2], parts[3], parts.get(4).copied().unwrap_or(""));
                            let key = parts.get(5).copied().unwrap_or("");
                            let (key_val, env_val) = match key.strip_prefix("env:") {
                                Some(env) => ("".to_string(), env.to_string()),
                                None => (key.to_string(), String::new()),
                            };
                            let mut cfg = config::load().unwrap_or_default();
                            match config::add_provider(&mut cfg, name, url, &key_val, &env_val, model) {
                                Ok(()) => println!("[added provider {name} ({url}, model {model}); now default]"),
                                Err(e) => println!("[add provider failed: {e:#}]"),
                            }
                        }
                        Some(p) => {
                            // Switch provider at runtime.
                            let provider = config::resolve_provider(&cfg, Some(p), None, None);
                            if provider.name != p {
                                println!("[provider {p} not found — see /provider]");
                                continue;
                            }
                            let client = match byteai_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
                                Ok(c) => c,
                                Err(e) => {
                                    println!("[provider {p}: failed: {e:#}]");
                                    continue;
                                }
                            };
                            let model = config::resolve_model(&cfg, None, &provider);
                            agent.pool.replace_active(&p, client, &model);
                            agent.config.model = model.clone();
                            let mut cfg2 = config::load().unwrap_or_default();
                            let _ = config::set_default_provider(&mut cfg2, p);
                            println!("[provider -> {p}, model -> {model}]");
                        }
                        None => {
                            println!("[provider = {}]", cfg.agent.default_provider);
                            println!("[configured providers:]");
                            for prov in &cfg.providers {
                                let cur = if prov.name == cfg.agent.default_provider { " ▸" } else { "  " };
                                let key = if prov.resolved_key().is_empty() { " (no key)" } else { "" };
                                println!("  {cur} {}{key}", prov.name);
                            }
                            println!("[switch: /provider <name> · add: /provider add <name> <url> <model> [key]]");
                        }
                    }
                }
                "tools" => {
                    println!("[tools ({})] {}", agent.tools.names().len(), agent.tools.names().join(", "));
                }
                "save" => {
                    if let Some(name) = cmd.split_whitespace().nth(1) {
                        let mut sf = session::from_agent(&agent.config.model, "repl", agent.history.clone(), agent.usage.clone());
                        sf.id = name.to_string();
                        match session::save(&sf) {
                            Ok(path) => println!("[session saved: {}]", path.display()),
                            Err(e) => println!("[save failed: {e:#}]"),
                        }
                    } else {
                        println!("[usage: /save <name>]");
                    }
                }
                "clear" | "new" => {
                    agent.history.retain(|m| m.role == byteai_types::Role::System);
                    println!("[conversation cleared]");
                }
                "usage" => {
                    println!("[usage: {} total tokens]", agent.usage.total_tokens);
                }
                "route" => {
                    let w: Vec<&str> = cmd.split_whitespace().collect();
                    let task_type = w.get(1).copied().unwrap_or("chat");
                    let task = w.get(2..).unwrap_or(&[]).join(" ");
                    let args = serde_json::json!({"type": task_type, "task": task});
                    match agent.tools.get("route") {
                        Some(t) => {
                            let outcome = t.execute(args).await;
                            println!("[route] {}", outcome.output);
                        }
                        None => println!("[route tool not available]"),
                    }
                }
                "council" => {
                    let question = cmd.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                    if question.is_empty() {
                        println!("[usage: /council <question>]");
                    } else {
                        let args = serde_json::json!({"question": question});
                        match agent.tools.get("council") {
                            Some(t) => {
                                let outcome = t.execute(args).await;
                                println!("[council] {}", outcome.output);
                            }
                            None => println!("[council tool not available]"),
                        }
                    }
                }
                "govern" => {
                    let action = cmd.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                    if action.is_empty() {
                        println!("[usage: /govern <action>]");
                    } else {
                        let args = serde_json::json!({"action": action});
                        match agent.tools.get("govern") {
                            Some(t) => {
                                let outcome = t.execute(args).await;
                                println!("[govern] {}", outcome.output);
                            }
                            None => println!("[govern tool not available]"),
                        }
                    }
                }
                "ideas" => {
                    let focus = cmd.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                    if focus.is_empty() {
                        println!("[usage: /ideas <focus> — e.g. AI + SaaS, developer tools, healthcare automation]");
                        println!("        /ideas menu · /ideas research <idea> · /ideas build <idea> · /ideas status");
                    } else {
                        let action = if focus == "menu" || focus == "help" { "menu" }
                            else if focus.starts_with("research") { "research" }
                            else if focus.starts_with("build") { "build" }
                            else if focus == "status" { "status" }
                            else { "discover" };
                        let query = if action == "discover" { focus.clone() }
                            else { focus.split_once(' ').map(|x| x.1).unwrap_or("").to_string() };
                        let args = serde_json::json!({"action": action, "focus": query});
                        match agent.tools.get("ideas") {
                            Some(t) => {
                                let outcome = t.execute(args).await;
                                println!("[ideas] {}", outcome.output);
                            }
                            None => println!("[ideas tool not available]"),
                        }
                    }
                }
                "github" => {
                    let rest = cmd.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                    if rest.is_empty() {
                        println!("[usage: /github <skills|harnesses|tools|mcp|improve|current|evaluate repo|search ...>]");
                        println!("        /github menu");
                    } else {
                        let first = cmd.split_whitespace().nth(1).unwrap_or("").to_string();
                        let action = match first.as_str() {
                            "menu"|"search"|"evaluate"|"improve"|"current"|"memory"|"graph"|"connect"|"status"|"push" => first.clone(),
                            _ => "search".to_string(),
                        };
                        let args = serde_json::json!({"action": action, "target": first, "query": rest});
                        match agent.tools.get("github") {
                            Some(t) => {
                                let outcome = t.execute(args).await;
                                println!("[github] {}", outcome.output);
                            }
                            None => println!("[github tool not available]"),
                        }
                    }
                }
                "cap" => {
                    agent.config.cap_enabled = !agent.config.cap_enabled;
                    let mut cfg = config::load().unwrap_or_default();
                    let _ = config::set_cap(&mut cfg, agent.config.cap_enabled);
                    println!("[CAP (Coding Auto-Pilot) -> {}]", if agent.config.cap_enabled { "ON — full autonomy, no stopping for questions" } else { "OFF — waits for your answer at questions" });
                }
                "settings" => {
                    println!("[model={}, tools={}, provider tokens={}]", agent.config.model, agent.tools.names().len(), agent.usage.total_tokens);
                }
                "setup" => {
                    if let Err(e) = setup::run() {
                        println!("[setup failed: {e:#}]");
                    }
                }
                "quit" | "q" | "exit" => break,
                _ => println!("unknown command /{cmd} — try /help"),
            }
            continue;
        }
        let (outcome, turn_tools, turn_ms) = run_turn(agent, &full).await?;
        if outcome.needs_input {
            // The agent asked a question and is waiting. Do NOT print the
            // "done/blocked" report — the next line the user types is their
            // answer, and it continues the same task (history already holds
            // the question).
            println!();
            println!("[✋ byteai is waiting for your answer — type it and press Enter to continue]");
            continue;
        }
        report(&outcome, agent, &turn_tools, turn_ms);
    }
    Ok(())
}

#[derive(clap::Args, Default)]
struct GithubArgs {
    /// Action: connect [repo] [public|private] | status | push | menu | search <target> <query> | evaluate <repo> | current | improve | memory | graph
    args: Vec<String>,
}

/// `byteai github …` — GitHub integration (mirror of `/github`).
async fn github_cmd(a: GithubArgs) -> Result<()> {
    let args = a.args.clone();
    let action = args.first().map(|s| s.as_str()).unwrap_or("menu");
    let (action, target, query) = if action == "connect" || action == "status" || action == "push" {
        // connect [repo] [public|private] → action=connect, target=connect, query=rest
        let rest = args[1..].join(" ");
        (action.to_string(), action.to_string(), rest)
    } else if action == "menu" || action == "memory" || action == "graph" || action == "current" {
        (action.to_string(), action.to_string(), String::new())
    } else if action == "evaluate" || action == "improve" || action == "search" {
        let rest = args[1..].join(" ");
        let first = args.get(1).cloned().unwrap_or_default();
        (action.to_string(), first, rest)
    } else {
        // Bare `byteai github` with a target like `skills <query>` → search.
        ("search".to_string(), args[0].clone(), args.join(" "))
    };

    let data_dir = config::data_dir();
    let lsp = Arc::new(byteai_lsp::LspRegistry::new(byteai_lsp::default_servers()));
    let dap = Arc::new(byteai_dap::DapRegistry::new(byteai_dap::default_adapters()));
    let mut ctx = ToolContext::with_all(data_dir.clone(), lsp, dap);
    if let Ok(cfg) = config::load() {
        let provider = config::resolve_provider(&cfg, None, None, None);
        let model = config::resolve_model(&cfg, None, &provider);
        if let Ok(client) = Client::new(provider.base_url.clone(), provider.resolved_key()) {
            ctx = ctx.with_provider(client, model);
        }
    }
    let tool = byteai_tools::github::GithubTool::new(ctx);
    let outcome = tool
        .execute(serde_json::json!({"action": action, "target": target, "query": query}))
        .await;
    println!("{}", outcome.output);
    if !outcome.ok {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(clap::Args, Default)]
struct MemoryArgs {
    /// Subcommand: status | setup | search | capture | skills | persona | scenario
    #[command(subcommand)]
    cmd: Option<MemoryCmd>,
}
#[derive(Subcommand)]
enum MemoryCmd {
    /// Show the native memory hub status (enabled, db path, per-layer counts).
    Status,
    /// One-shot enable: set [memory] enabled=true in config.toml.
    Setup,
    /// Search the native hub (L1 atomics + L0 conversation + skills).
    Search { query: String },
    /// Capture a message pair into L0 (session, user, assistant).
    Capture {
        #[arg(long, default_value = "default-conversation")]
        session: String,
        user: String,
        assistant: Option<String>,
    },
    /// List skills in the hub's skill memory.
    Skills,
    /// Write the L3 core persona.
    Persona { text: String },
    /// Write an L2 scenario file.
    Scenario { path: String, content: String },
}

/// ByteAi memory hub management (`byteai memory ...`). Native, local-first —
/// no external service. All data lives in data_dir/memory/memory.db.
async fn memory_cmd(args: MemoryArgs) -> Result<()> {
    let cfg = config::load()?;
    let data_dir = config::data_dir().join("memory");
    let enabled = cfg.memory.enabled;

    match args.cmd {
        Some(MemoryCmd::Status) | None => {
            let hub = match byteai_memory::hub::MemoryHub::open(&data_dir) {
                Ok(h) => h,
                Err(e) => anyhow::bail!("cannot open memory hub at {}: {e}", data_dir.display()),
            };
            println!("memory hub: {} ({})", if enabled { "ENABLED" } else { "disabled — run `byteai memory setup`" }, data_dir.join("memory.db").display());
            match hub.stats() {
                Ok(stats) => {
                    for (label, count) in &stats {
                        println!("  {label}: {count}");
                    }
                }
                Err(e) => println!("  stats error: {e}"),
            }
            if let Ok(Some(core)) = hub.core_read() {
                println!("  l3 persona: {} (v{})", core.content.chars().take(60).collect::<String>(), core.version);
            } else {
                println!("  l3 persona: (not set — use `byteai memory persona \"...\"`)");
            }
        }
        Some(MemoryCmd::Setup) => {
            let mut cfg = config::load()?;
            cfg.memory.enabled = true;
            config::save(&cfg)?;
            println!("✓ memory hub enabled — [memory] enabled = true in {}", config::path().display());
            println!("  Data lives in {}", data_dir.join("memory.db").display());
            println!("  Every byteai turn now captures L0 dialogue and recalls L1/L2/L3 + skills.");
        }
        Some(MemoryCmd::Search { query }) => {
            if !enabled {
                anyhow::bail!("memory not enabled — run `byteai memory setup` first");
            }
            let hub = byteai_memory::hub::MemoryHub::open(&data_dir)?;
            println!("L1 atomics:");
            match hub.atomic_search(&query, 10) {
                Ok(items) if !items.is_empty() => {
                    for it in items {
                        println!("  • [{}] {}", it.mem_type, it.content);
                    }
                }
                Ok(_) => println!("  (none)"),
                Err(e) => println!("  error: {e}"),
            }
            println!("L0 conversation:");
            match hub.conversation_search(&query, 5) {
                Ok(items) if !items.is_empty() => {
                    for it in items {
                        println!("  • [{}] {}", it.role, it.content);
                    }
                }
                Ok(_) => println!("  (none)"),
                Err(e) => println!("  error: {e}"),
            }
            println!("skills:");
            match hub.skill_search(&query, 5) {
                Ok(items) if !items.is_empty() => {
                    for it in items {
                        println!("  • {} (v{})", it.name, it.version);
                    }
                }
                Ok(_) => println!("  (none)"),
                Err(e) => println!("  error: {e}"),
            }
        }
        Some(MemoryCmd::Capture { session, user, assistant }) => {
            if !enabled {
                anyhow::bail!("memory not enabled — run `byteai memory setup` first");
            }
            let mut hub = byteai_memory::hub::MemoryHub::open(&data_dir)?;
            let mut msgs: Vec<(&str, &str)> = vec![("user", &user)];
            if let Some(a) = &assistant {
                msgs.push(("assistant", a));
            }
            match hub.conversation_add(&session, &msgs) {
                Ok(n) => println!("captured {n} message(s) → session {session}"),
                Err(e) => anyhow::bail!("capture failed: {e}"),
            }
        }
        Some(MemoryCmd::Skills) => {
            if !enabled {
                anyhow::bail!("memory not enabled — run `byteai memory setup` first");
            }
            let hub = byteai_memory::hub::MemoryHub::open(&data_dir)?;
            match hub.skill_list(50) {
                Ok(items) if !items.is_empty() => {
                    for it in items {
                        println!("  • {} (v{})", it.name, it.version);
                    }
                }
                Ok(_) => println!("  (no skills in hub — use the `skills` tool or add SKILL.md files)"),
                Err(e) => println!("  error: {e}"),
            }
        }
        Some(MemoryCmd::Persona { text }) => {
            if !enabled {
                anyhow::bail!("memory not enabled — run `byteai memory setup` first");
            }
            let mut hub = byteai_memory::hub::MemoryHub::open(&data_dir)?;
            match hub.core_write(&text) {
                Ok(()) => println!("✓ L3 core persona written (v{})", hub.core_read().ok().flatten().map(|c| c.version).unwrap_or(1)),
                Err(e) => anyhow::bail!("persona write failed: {e}"),
            }
        }
        Some(MemoryCmd::Scenario { path, content }) => {
            if !enabled {
                anyhow::bail!("memory not enabled — run `byteai memory setup` first");
            }
            let mut hub = byteai_memory::hub::MemoryHub::open(&data_dir)?;
            match hub.scenario_write(&path, &content, None) {
                Ok(()) => println!("✓ L2 scenario written: {path}"),
                Err(e) => anyhow::bail!("scenario write failed: {e}"),
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn _keep(_: Arc<Registry>, _: &str) {
    let _ = SYSTEM_PROMPT;
    info!("");
    let _ = Message::system("x");
}
