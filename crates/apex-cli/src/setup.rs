//! `byteai setup` — interactive first-run wizard for providers, models,
//! skills, tools, and agent configuration. Zero external dependencies:
//! uses stdin/stdout line prompts.

use std::io::BufRead;

use anyhow::Result;

/// Run the interactive setup wizard. Walks through:
///   1. Welcome + existing config detection
///   2. Provider setup (name, base_url, api_key/env, model)
///   3. Agent settings (max_iterations, tool_timeout, CAP, memory, auto_continue)
///   4. Skills installation (optional)
///   5. Doctor verification
///   6. Summary
pub fn run() -> Result<()> {
    println!();
    println!("╔══════════════════════════════════════════════════╗");
    println!("║     ByteAi Setup Wizard  —  interactive setup   ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();
    println!("This wizard will configure ByteAI step by step.");
    println!("Press Enter to accept defaults shown in [brackets].");
    println!();

    // 1. Detect existing config.
    let existing = std::fs::read_to_string(crate::config::path()).ok();
    let has_config = existing.as_ref().map(|s| s.trim().len() > 20).unwrap_or(false);
    if has_config {
        println!("✓ Existing config found at: {}", crate::config::path().display());
        let proceed = ask("Would you like to overwrite it?", "n", &["y", "n"]);
        if proceed.as_deref().unwrap_or("n") != "y" {
            println!("Setup cancelled. Config unchanged.");
            return Ok(());
        }
    }

    let mut cfg = crate::config::Config::default();

    // 2. Provider setup.
    println!();
    println!("── Provider — any OpenAI-compatible API endpoint ──");
    println!();
    let name = ask("Provider name", "omniroute", &[]);
    let name = name.as_deref().unwrap_or("omniroute");
    let base_url = ask("Base URL", "http://localhost:20128/v1", &[]);
    let base_url = base_url.as_deref().unwrap_or("http://localhost:20128/v1");
    let model = ask("Default model", "deepseek-v4-flash", &[]);
    let model = model.as_deref().unwrap_or("deepseek-v4-flash");
    println!();
    println!("API key: leave empty for local providers, or set:");
    println!("  (1) Paste a literal key");
    println!("  (2) Use an environment variable name");
    let key_mode = ask("Option", "1", &["1", "2"]);
    let key_mode = key_mode.as_deref().unwrap_or("1");
    let (api_key, api_key_env) = if key_mode == "1" {
        let key = ask("API key (or leave empty)", "", &[]).unwrap_or_default();
        (key, String::new())
    } else {
        let env = ask("Env var name", "MY_API_KEY", &[]);
        let env = env.unwrap_or_else(|| "MY_API_KEY".to_string());
        (String::new(), env)
    };

    cfg.providers.push(crate::config::ProviderEntry {
        name: name.to_string(),
        base_url: base_url.to_string(),
        api_key,
        api_key_env,
        model: model.to_string(),
    });
    cfg.agent.default_provider = name.to_string();

    // 2b. Optional second provider.
    let add_second = ask("Add a second provider? (useful for failover)", "n", &["y", "n"]);
    if add_second.as_deref().unwrap_or("n") == "y" {
        let name2 = ask("Second provider name", "bai", &[]);
        let name2 = name2.as_deref().unwrap_or("bai");
        let base_url2 = ask("Base URL", "https://api.b.ai/v1", &[]);
        let base_url2 = base_url2.as_deref().unwrap_or("https://api.b.ai/v1");
        let model2 = ask("Model", "deepseek-v4-flash", &[]);
        let model2 = model2.as_deref().unwrap_or("deepseek-v4-flash");
        let key2 = ask("API key (or env var name)", "", &[]).unwrap_or_default();
        let (ak2, env2) = if key2.contains('_') && !key2.starts_with("sk-") {
            (String::new(), key2)
        } else {
            (key2, String::new())
        };
        cfg.providers.push(crate::config::ProviderEntry {
            name: name2.to_string(),
            base_url: base_url2.to_string(),
            api_key: ak2,
            api_key_env: env2,
            model: model2.to_string(),
        });
    }

    // 3. Agent settings.
    println!();
    println!("── Agent Behaviour ──");
    println!();
    let max_iters = ask("Max iterations per turn", "300", &[]);
    let max_iters: u32 = max_iters.as_deref().unwrap_or("300").parse().unwrap_or(300);
    let tool_timeout = ask("Tool timeout (seconds, 0 = default 300)", "300", &[]);
    let tool_timeout_secs: u64 = tool_timeout.as_deref().unwrap_or("300").parse().unwrap_or(300);
    let auto_continue = ask("Auto-continue mid-task? (Y/n)", "y", &["y", "n"]);
    let cap = ask("CAP — Coding Auto-Pilot? (full autonomy, no pauses for questions)", "n", &["y", "n"]);
    let memory = ask("Enable memory (persistent context across sessions)?", "y", &["y", "n"]);
    let tool_select = ask("Smart tool selection (only expose relevant tools per turn)?", "y", &["y", "n"]);

    cfg.agent.max_iterations = max_iters;
    if tool_timeout_secs > 0 {
        cfg.agent.tool_timeout_seconds = Some(tool_timeout_secs);
    }
    cfg.agent.auto_continue = auto_continue.as_deref().unwrap_or("y") == "y";
    cfg.agent.cap_enabled = cap.as_deref().unwrap_or("n") == "y";
    cfg.memory.enabled = memory.as_deref().unwrap_or("y") == "y";
    cfg.agent.tool_select = tool_select.as_deref().unwrap_or("y") == "y";

    // 4. Write config.
    let dir = crate::config::config_dir();
    std::fs::create_dir_all(&dir)?;
    crate::config::save(&cfg)?;
    println!();
    println!("✓ Config written to: {}", crate::config::path().display());

    // 5. Skills installation.
    println!();
    println!("── Skills ──");
    let install_skills = ask("Install starter skills? (recommended)", "y", &["y", "n"]);
    if install_skills.as_deref().unwrap_or("n") == "y" {
        println!("Installing built-in skills…");
        run_skills_setup()?;
    }

    // 6. Doctor verification.
    println!();
    println!("── Verification ──");
    let run_doctor = ask("Run provider connectivity check? (recommended)", "y", &["y", "n"]);
    if run_doctor.as_deref().unwrap_or("y") == "y" {
        run_doctor_check()?;
    }

    // 7. Summary.
    println!();
    println!("╔══════════════════════════════════════════════════╗");
    println!("║     Setup complete!                              ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();
    println!("Next steps:");
    println!("  byteai          — launch the TUI");
    println!("  byteai chat     — REPL mode");
    println!("  byteai doctor   — check provider connectivity");
    println!("  byteai models   — list available models");
    println!("  /github connect — publish this project to GitHub");
    println!();
    Ok(())
}

/// Interactive prompt: print question + default, read line.
fn ask(question: &str, default: &str, choices: &[&str]) -> Option<String> {
    let choices_hint = if choices.is_empty() {
        String::new()
    } else {
        format!(" ({})", choices.join("/"))
    };
    print!("  {}{} [{}]: ", question, choices_hint, default);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).ok()?;
    if read == 0 {
        return None; // EOF
    }
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        Some(default.to_string())
    } else {
        Some(trimmed)
    }
}

/// Simple skills setup: ensure the default skills directory exists.
fn run_skills_setup() -> Result<()> {
    let data_dir = crate::config::data_dir();
    let skills_dir = data_dir.join("skills");
    std::fs::create_dir_all(&skills_dir)?;

    let sample = skills_dir.join("README.md");
    if !sample.exists() {
        std::fs::write(
            &sample,
            "# ByteAi Skills\n\nPlace SKILL.md files in this directory to add skills.\n\
             See https://github.com/ByteCyberSec/byteai for documentation.\n",
        )?;
    }
    println!("  ✓ Skills directory: {}", skills_dir.display());
    Ok(())
}

/// Run the doctor check loop.
fn run_doctor_check() -> Result<()> {
    println!("  Checking providers…");
    let cfg = crate::config::load()?;
    let mut any_ok = false;
    for p in &cfg.providers {
        let key = p.resolved_key();
        let has_key = !key.is_empty();
        let client = match apex_provider::Client::new(p.base_url.clone(), key) {
            Ok(c) => c,
            Err(e) => {
                println!("  ✗ [{}] build client failed: {e:#}", p.name);
                continue;
            }
        };
        println!("  Testing {} → {} …", p.name, p.base_url);
        // The wizard runs inside the tokio main runtime; spawn a dedicated
        // thread with its own runtime so block_on is legal here.
        let name = p.name.clone();
        let result = std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => return Err(format!("runtime error: {e}")),
            };
            rt.block_on(client.list_models())
                .map_err(|e| format!("{e:#}"))
        })
        .join()
        .unwrap_or_else(|_| Err("doctor thread panicked".to_string()));
        match result {
            Ok(ids) => {
                any_ok = true;
                println!("  ✓ [{name}] OK — {} models available", ids.len());
                if !has_key {
                    println!("      (no API key — unauthenticated endpoint)");
                }
            }
            Err(e) => {
                println!("  ✗ [{name}] {e}");
            }
        }
    }
    if !any_ok {
        println!("  ⚠ No reachable provider. Set BYTEAI_BASE_URL / BYTEAI_API_KEY");
        println!("     or edit: {}", crate::config::path().display());
    }
    Ok(())
}