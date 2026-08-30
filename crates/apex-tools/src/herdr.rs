//! Herdr integration: when ByteAI runs inside a Herdr terminal, this module
//! lets it spawn sub-agents in NEW Herdr TABS instead of background processes.
//!
//! Herdr is a terminal multiplexer for coding agents
//! (https://github.com/herdrdev/herdr). When `HERDR_ENV=1` is set, ByteAI is
//! running inside a Herdr pane and can control the Herdr session via CLI.
//!
//! Workspace strategy: ByteAI NEVER creates a new Herdr workspace. Each
//! sub-agent is spawned directly under the CALLER'S CURRENT workspace — the
//! one ByteAI is already running in (resolved from `HERDR_WORKSPACE_ID`, or
//! Herdr's default when the env var is absent). This keeps the whole team
//! side by side in one place — visible in the Herdr TUI — while the main
//! agent monitors them from its own pane.
//!
//! Spawn strategy (one tab per sub-agent, named after its task):
//!   1. `herdr tab create [--workspace <ws>] --cwd <cwd> --label <short-task-name> --no-focus`
//!      → returns a NEW tab with its own root pane. The tab is visible in the
//!      Herdr TUI while the sub-agent works, labeled with a short slug of the
//!      goal — not a split panel crammed into the caller's tab.
//!   2. `herdr pane run <root_pane> "<cmd> > <log> 2>&1; printf 'DONE_<id>'"`
//!      — child output goes to a real file (NOT the alternate screen), and a
//!      unique DONE marker prints to the normal screen when it exits.
//!   3. `herdr pane wait-output <root_pane> --match "DONE_<id>" --timeout <ms>`
//!   4. Read the log file for the complete output.
//!   5. Close the tab on success (cleanup); leave it open on timeout so the
//!      user can inspect/kill a stuck agent.
//!
//! Fallback: when Herdr is not available, the caller falls back to raw
//! `Command::spawn` (the original background-process approach).

use std::process::Output;

/// Whether the current process is inside a Herdr-managed pane.
pub fn is_inside_herdr() -> bool {
    std::env::var("HERDR_ENV").map(|v| v == "1").unwrap_or(false)
}

/// Get the caller's pane ID from the environment.
pub fn caller_pane_id() -> Option<String> {
    std::env::var("HERDR_PANE_ID").ok()
}

/// Get the caller's tab ID.
pub fn caller_tab_id() -> Option<String> {
    std::env::var("HERDR_TAB_ID").ok()
}

/// Get the caller's workspace ID.
pub fn caller_workspace_id() -> Option<String> {
    std::env::var("HERDR_WORKSPACE_ID").ok()
}

/// Create a NEW Herdr workspace — **NOT used by spawn anymore**. ByteAI
/// intentionally does NOT create workspaces: sub-agents always spawn under
/// the caller's current workspace. Kept only as a generic helper.
///
/// Uses `--no-focus` so creating the workspace never steals the operator's
/// current pane.
pub async fn create_project_workspace(label: &str, cwd: &str) -> Result<String, String> {
    let cmd = format!(
        "herdr workspace create --cwd {} --label {} --no-focus",
        shell_quote(cwd),
        shell_quote(label),
    );
    let out = run_cli(&cmd).await?;
    parse_workspace_created(&out)
}

/// Parse the workspace_id from a `herdr workspace create` JSON response.
/// Response shape: { "result": { "workspace": { "workspace_id": "w7", ... } } }
fn parse_workspace_created(json: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("could not parse workspace create response ({e}): {json}"))?;
    let ws_id = v
        .get("result")
        .and_then(|r| r.get("workspace"))
        .and_then(|w| w.get("workspace_id"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("no workspace_id in response: {json}"))?;
    Ok(ws_id.to_string())
}

/// Spawn a sub-agent in a NEW Herdr tab (named after its task), run it, wait
/// for completion, and read its output. Returns the output as if it were a
/// `Command::Output`.
///
/// Strategy (robust against Herdr's alternate-screen limitation):
/// 1. `herdr tab create` → a new tab with its own root pane, labeled with a
///    short slug of the task goal (visible in the TUI while it works).
/// 2. Run `<command> <args...> > <logfile> 2>&1; printf 'HERDR_DONE_<id>'` in
///    the tab's root pane — output goes to a real file (NOT the alternate
///    screen), and a unique DONE marker prints to the normal screen.
/// 3. `herdr pane wait-output` for the marker (so we know it truly finished).
/// 4. Read the log file for the complete output.
/// 5. Close the tab on success; leave it open on timeout (user can kill).
pub async fn spawn_pane(
    idx: usize,
    command: &str,
    args: &[String],
    timeout_secs: u64,
    workspace_id: Option<&str>,
) -> Result<(Output, String), String> {
    if !is_inside_herdr() {
        return Err("not inside Herdr".into());
    }

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    // Step 1: create a NEW TAB (not a split panel) named after the task.
    // Spawn directly under the caller's CURRENT workspace — ByteAI never
    // creates a new workspace. `workspace_id` is optional: when given it is
    // used explicitly (spawn.rs passes the caller's own workspace id);
    // otherwise we fall back to the caller's workspace from the env.
    let label = task_label(idx, args);
    let ws_owned: Option<String> = workspace_id.map(|s| s.to_string()).or_else(caller_workspace_id);
    let create_cmd = match &ws_owned {
        Some(ws) => format!(
            "herdr tab create --workspace {} --cwd {} --label {} --no-focus",
            shell_quote(ws),
            shell_quote(&cwd),
            shell_quote(&label),
        ),
        None => format!(
            "herdr tab create --cwd {} --label {} --no-focus",
            shell_quote(&cwd),
            shell_quote(&label),
        ),
    };
    let create_out = run_cli(&create_cmd).await?;
    let (tab_id, pane_id) = parse_tab_created(&create_out)?;

    // Unique id for this spawn (used for the DONE marker + log file name).
    let unique = format!("{}", std::process::id());
    let log_path = std::env::temp_dir()
        .join(format!("byteai_herdr_{}_{}.log", unique, std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
    let log_str = log_path.to_string_lossy().to_string();
    let done_marker = format!("HERDR_DONE_{unique}");

    // Step 2: run the command with output redirected to a file, then print a
    // unique DONE marker to the normal screen.
    let full_cmd = build_run_command(&pane_id, &log_str, &done_marker, command, args);
    let _ = run_cli(&full_cmd).await?;

    // Step 3: wait for the DONE marker (normal-screen output, so Herdr can
    // actually see it — unlike the child's TUI which uses the alternate screen).
    let wait_cmd = format!(
        "herdr pane wait-output {} --match \"{}\" --timeout {}",
        pane_id,
        done_marker,
        timeout_secs * 1000
    );
    let wait_out = run_cli(&wait_cmd).await;
    let completed = matches!(&wait_out, Ok(w) if w.contains(&done_marker));
    let read_out = std::fs::read_to_string(&log_str).unwrap_or_default();
    let _ = std::fs::remove_file(&log_str);

    // Step 5: cleanup. Close the tab when the task finished; leave it open on
    // timeout so the user can inspect/kill a stuck agent in its named tab.
    let note = if completed {
        let _ = run_cli(&format!("herdr tab close {}", tab_id)).await;
        String::new()
    } else {
        format!(
            "\n[tab {} left open: no completion marker within {}s — inspect or kill it in Herdr]",
            tab_id, timeout_secs
        )
    };

    // Build a fake Output for the caller.
    let mut stdout = read_out;
    if !note.is_empty() {
        stdout.push_str(&note);
    }
    let output = Output {
        status: std::process::ExitStatus::default(),
        stdout: stdout.into_bytes(),
        stderr: Vec::new(),
    };
    Ok((output, pane_id))
}

/// Run a `herdr` CLI command and return its stdout.
async fn run_cli(cmd: &str) -> Result<String, String> {
    let output = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| format!("herdr CLI error: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(format!("herdr CLI failed: {stderr}\n{stdout}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse the tab + root-pane IDs from a `herdr tab create` JSON response.
/// Response shape: { "result": { "tab": { "tab_id": "w1:t3" },
///                               "root_pane": { "pane_id": "w1:p4" } } }
fn parse_tab_created(json: &str) -> Result<(String, String), String> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| format!("could not parse tab create response ({e}): {json}"))?;
    let tab_id = v
        .get("result")
        .and_then(|r| r.get("tab"))
        .and_then(|t| t.get("tab_id"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("no tab_id in response: {json}"))?;
    let pane_id = v
        .get("result")
        .and_then(|r| r.get("root_pane"))
        .and_then(|p| p.get("pane_id"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("no root_pane in response: {json}"))?;
    Ok((tab_id.to_string(), pane_id.to_string()))
}

/// Derive a short, TUI-safe tab label from the sub-agent's goal (the last
/// CLI arg), prefixed with the agent index so parallel tabs are distinct.
/// Example: (3, "implement the login flow") → "a3-implement-the-login".
fn task_label(idx: usize, args: &[String]) -> String {
    let goal = args.last().map(|s| s.as_str()).unwrap_or("task");
    let slug: String = goal
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c.is_whitespace() || c == '-' || c == '_' {
                Some('-')
            } else {
                None
            }
        })
        .collect();
    // Collapse runs of dashes, trim leading/trailing dashes.
    let mut out = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash && !out.is_empty() {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    let slug = out.trim_matches('-').to_string();
    let slug: String = slug.chars().take(24).collect();
    let base = if slug.is_empty() { "task".to_string() } else { slug };
    format!("a{idx}-{base}")
}

/// Build a `herdr pane run` command string that runs the given command with
/// output redirected to a log file, then emits a unique DONE marker to the
/// normal screen (so Herdr's wait-output can detect completion).
/// Uses the documented syntax: `herdr pane run <pane> "<full command string>"`.
///
/// IMPORTANT: the marker must NOT appear verbatim in the typed command,
/// because Herdr's wait-output sees the echoed command line too — it would
/// match immediately, before the child finished. We emit the marker with
/// `printf 'HERDR_DONE_%s' <id>` so the command text differs from the
/// actual output (`HERDR_DONE_%s` vs `HERDR_DONE_<id>`).
fn build_run_command(
    pane_id: &str,
    log_path: &str,
    done_marker: &str,
    command: &str,
    args: &[String],
) -> String {
    // Build the inner command: <command> <args...> > <log> 2>&1; printf marker
    let mut inner = shell_quote(command);
    for a in args {
        inner.push(' ');
        inner.push_str(&shell_quote(a));
    }
    // Split the marker "HERDR_DONE_<id>" at the last underscore: prefix is
    // the printf format text, suffix is the id printed separately.
    let (prefix, id) = match done_marker.rfind('_') {
        Some(i) => (&done_marker[..i + 1], &done_marker[i + 1..]),
        None => (done_marker, ""),
    };
    let full = format!(
        "{} > {} 2>&1; printf '{}%s\\n' {}",
        inner,
        shell_quote(log_path),
        prefix,
        id
    );
    // Pass the whole command as a single quoted argument to pane run.
    format!("herdr pane run {} {}", pane_id, shell_quote(&full))
}

/// Shell-quote a string for safe interpolation.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_handles_simple_paths() {
        assert_eq!(shell_quote("/tmp/test"), "/tmp/test");
        assert_eq!(shell_quote("byteai"), "byteai");
    }

    #[test]
    fn shell_quote_handles_spaces() {
        assert_eq!(shell_quote("/path/with spaces/file"), "'/path/with spaces/file'");
    }

    #[test]
    fn shell_quote_handles_single_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_empty() {
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn parse_tab_created_from_json_works() {
        let json = r#"{"id":"cli:tab:create","result":{"root_pane":{"pane_id":"w5:p2W"},"tab":{"tab_id":"w5:tD"}}}"#;
        let (tab, pane) = parse_tab_created(json).unwrap();
        assert_eq!(tab, "w5:tD");
        assert_eq!(pane, "w5:p2W");
    }

    #[test]
    fn parse_tab_created_rejects_garbage() {
        assert!(parse_tab_created("not json").is_err());
        assert!(parse_tab_created(r#"{"result":{}}"#).is_err());
    }

    #[test]
    fn parse_workspace_created_from_json_works() {
        let json = r#"{"id":"cli:workspace:create","result":{"workspace":{"workspace_id":"w7","label":"byteai-probe","number":2,"pane_count":1,"tab_count":1}}}"#;
        assert_eq!(parse_workspace_created(json).unwrap(), "w7");
    }

    #[test]
    fn parse_workspace_created_rejects_garbage() {
        assert!(parse_workspace_created("not json").is_err());
        assert!(parse_workspace_created(r#"{"result":{}}"#).is_err());
        assert!(parse_workspace_created(r#"{"result":{"workspace":{}}}"#).is_err());
    }

    #[test]
    fn task_label_slugs_goal() {
        let args = vec!["chat".into(), "implement the login flow and wire it to the api".into()];
        assert_eq!(task_label(0, &args), "a0-implement-the-login-flow");
        // A long goal gets truncated to 24 chars, then prefixed.
        let args = vec!["chat".into(), "fix the critical payment gateway bug in production now please".into()];
        assert_eq!(task_label(1, &args), "a1-fix-the-critical-payment");
        // Non-alphanumerics collapse to single dashes, index helps distinguish.
        let args = vec!["chat".into(), "FIX  BUG   #42   !!!".into()];
        assert_eq!(task_label(2, &args), "a2-fix-bug-42");
        let args = vec!["chat".into(), "   ".into()];
        assert_eq!(task_label(3, &args), "a3-task");
        assert_eq!(task_label(5, &[]), "a5-task");
    }

    #[test]
    fn is_inside_herdr_reflects_env() {
        let original = std::env::var("HERDR_ENV").ok();
        // Set to 1 -> true
        unsafe { std::env::set_var("HERDR_ENV", "1"); }
        assert!(is_inside_herdr());
        // Set to 0 -> false
        unsafe { std::env::set_var("HERDR_ENV", "0"); }
        assert!(!is_inside_herdr());
        // Restore original value.
        match original {
            Some(v) => unsafe { std::env::set_var("HERDR_ENV", v); },
            None => unsafe { std::env::remove_var("HERDR_ENV"); },
        }
    }
}
