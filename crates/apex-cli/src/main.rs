//! ByteAi (codename APEX) — Phase 1 CLI.
//!
//! Subcommands: chat (one-shot or REPL), session (save/load/list), doctor,
//! models, tui. Provider-agnostic over any OpenAI-compatible endpoint.

mod config;
mod session;
#[cfg(feature = "tui")]
mod tui;

use std::sync::Arc;

use anyhow::{Context, Result};
use apex_core::{Agent, AgentConfig, SYSTEM_PROMPT};
use apex_provider::Client;
use apex_tools::{Registry, ToolContext};
use apex_types::{Message, ToolOutcome};
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(name = "byteai", version, about = "ByteAi (codename APEX) — fast autonomous coding agent")]
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
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,byteai=info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
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
    }
}

/// Direct tool invocation (no LLM): `byteai tool <name> '<json args>'`.
async fn tool_cmd(ta: ToolArgs) -> Result<()> {
    let data_dir = config::data_dir();
    let lsp = Arc::new(apex_lsp::LspRegistry::new(apex_lsp::default_servers()));
    let dap = Arc::new(apex_dap::DapRegistry::new(apex_dap::default_adapters()));
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
fn build_agent(
    args: &ChatArgs,
    no_tools: bool,
) -> Result<(Agent, String, String)> {
    let cfg = config::load()?;
    let provider = config::resolve_provider(&cfg, args.provider.as_deref(), args.base_url.as_deref(), args.api_key.as_deref());
    let model = config::resolve_model(&cfg, args.model.as_deref(), &provider);
    let client = Client::new(provider.base_url.clone(), provider.resolved_key())?;
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
        ..AgentConfig::default()
    };
    let data_dir = config::data_dir();
    let lsp = Arc::new(apex_lsp::LspRegistry::new(apex_lsp::default_servers()));
    let dap = Arc::new(apex_dap::DapRegistry::new(apex_dap::default_adapters()));
    let tools = Registry::builtins(&ToolContext::with_all(data_dir.clone(), lsp, dap));
    let agent = Agent::new(client, agent_cfg, tools, data_dir.clone());
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
            let outcome = run_turn(&mut agent, &prompt).await?;
            report(&outcome, &agent);
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
            match apex_memory::Memory::open(&config::data_dir().join("memory")) {
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
    let lsp = apex_lsp::LspRegistry::new(apex_lsp::default_servers());
    let langs = lsp.available();
    println!();
    println!("LSP servers on PATH: {}", if langs.is_empty() { "none".into() } else { langs.join(", ") });
    for spec in apex_lsp::default_servers() {
        let on_path = apex_lsp::command_on_path(&spec.command);
        println!("  {:<10} {} {}", spec.lang, if on_path { "OK " } else { "MISSING" }, spec.command);
    }
    /// Command availability check (shared with LSP registry).
    let dap = apex_dap::DapRegistry::new(apex_dap::default_adapters());
    println!();
    println!("DAP adapters on PATH:");
    for spec in apex_dap::default_adapters() {
        let on_path = apex_dap::command_on_path(&spec.command);
        println!("  {:<10} {} {}{}", spec.lang, if on_path { "OK " } else { "MISSING" }, spec.command, if on_path && spec.adapter_id != "debugpy" { " (stdio)" } else { " (adapter)" });
    }
    // Memory + skills.
    match apex_memory::Memory::open(&config::data_dir().join("memory")) {
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

async fn run_turn(agent: &mut Agent, prompt: &str) -> Result<apex_types::AgentOutcome> {
    let mut text_sink = |t: &str| print!("{t}");
    let mut tool_sink = |o: &ToolOutcome| {
        let preview: String = o.output.chars().take(90).collect();
        let preview = preview.replace('\n', " ");
        println!();
        println!("  [tool] {} {} — {} ({} ms)", o.name, if o.ok { "✓" } else { "✗" }, preview, o.elapsed_ms);
    };
    let outcome = agent.run(prompt, &mut text_sink, &mut tool_sink).await?;
    println!();
    Ok(outcome)
}

fn report(outcome: &apex_types::AgentOutcome, agent: &Agent) {
    println!();
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

/// Interactive REPL with a small command surface.
async fn repl(agent: &mut Agent) -> Result<()> {
    println!("ByteAi REPL — model {} · {} tools. Type /help for commands.", agent.config.model, agent.tools.names().len());
    println!("(multiline: end a line with \\ )");
    let stdin = std::io::stdin();
    let mut buffer = String::new();
    loop {
        let mut line = String::new();
        use std::io::BufRead;
        let read = stdin.lock().read_line(&mut line)?;
        if read == 0 {
            println!();
            break; // EOF
        }
        let line = line.trim_end();
        if line.ends_with('\\') {
            buffer.push_str(&line[..line.len() - 1]);
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
                    println!("/save <name>    — save this session");
                    println!("/usage          — show token usage");
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
                            agent.provider = match apex_provider::Client::new(provider.base_url.clone(), provider.resolved_key()) {
                                Ok(c) => c,
                                Err(e) => {
                                    println!("[provider {p}: failed: {e:#}]");
                                    continue;
                                }
                            };
                            let model = config::resolve_model(&cfg, None, &provider);
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
                    agent.history.retain(|m| m.role == apex_types::Role::System);
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
                "settings" => {
                    println!("[model={}, tools={}, provider tokens={}]", agent.config.model, agent.tools.names().len(), agent.usage.total_tokens);
                }
                "quit" | "q" | "exit" => break,
                _ => println!("unknown command /{cmd} — try /help"),
            }
            continue;
        }
        let outcome = run_turn(agent, &full).await?;
        report(&outcome, agent);
    }
    Ok(())
}

#[allow(dead_code)]
fn _keep(_: Arc<Registry>, _: &str) {
    let _ = SYSTEM_PROMPT;
    let _ = info!("");
    let _ = Message::system("x");
}
