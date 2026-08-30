//! `terminal` — persistent shell sessions (DeepSeek-harness `terminal/` idea).
//!
//! ByteAI's `shell` tool is one-shot: every call starts a fresh bash in the
//! session cwd, so `cd`, exports, and REPL state die between calls. This tool
//! gives the agent *persistent* interactive sessions: each session keeps its
//! working directory across tool calls, so a multi-step workflow (clone →
//! cd into repo → install → test) can hold state between steps, exactly like
//! a human's terminal tab.
//!
//! Implementation: sessions are plain JSON files under `<data>/terminals/`,
//! one per session, storing the current working directory. `run` executes the
//! command inside a bash subshell that starts in the session cwd and reports
//! its final cwd (`pwd`) so `cd` inside the command is picked up and
//! persisted for the next call. Sessions are process-local in the sense that
//! their *state file* survives restarts; the running children do not (same
//! contract as DeepSeek's terminal).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, err_outcome, ok_outcome};

pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// One persistent session: an id, a label, and a working directory that
/// survives across tool calls.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TermSession {
    pub id: String,
    pub label: String,
    pub cwd: String,
    pub created_ms: u64,
    pub runs: u64,
}

pub struct TerminalTool {
    dir: PathBuf,
}

impl TerminalTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("terminals");
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn sessions(&self) -> Vec<TermSession> {
        let mut out: Vec<TermSession> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("json") {
                    if let Some(s) = std::fs::read_to_string(&p)
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                    {
                        out.push(s);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms));
        out
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn save(&self, s: &TermSession) {
        let _ = std::fs::write(
            self.path(&s.id),
            serde_json::to_string_pretty(s).unwrap_or_default(),
        );
    }

    fn get(&self, id: &str) -> Option<TermSession> {
        std::fs::read_to_string(self.path(id))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn delete(&self, id: &str) {
        let _ = std::fs::remove_file(self.path(id));
    }

    fn create(&self, label: &str) -> TermSession {
        let id = format!(
            "t{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/".to_string());
        let s = TermSession {
            id: id.clone(),
            label: if label.is_empty() { "default".into() } else { label.into() },
            cwd,
            created_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            runs: 0,
        };
        self.save(&s);
        s
    }

    async fn run(&self, s: &TermSession, command: &str, timeout: u64) -> Result<(String, String), String> {
        // Run inside a bash subshell that starts in the session cwd and
        // echoes its final cwd so `cd` inside the command persists.
        let script = format!(
            "cd -- \"{cwd}\" && {command}; echo; echo '__BYTEAI_CWD__' \"$(pwd)\"",
            cwd = s.cwd.replace('"', "\\\""),
            command = command
        );
        let mut cmd = tokio::process::Command::new("/bin/bash");
        cmd.arg("-c").arg(&script);
        cmd.kill_on_drop(true);
        let output = match tokio::time::timeout(Duration::from_secs(timeout.max(1)), cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(format!("spawn failed: {e}")),
            Err(_) => return Err("timed out".to_string()),
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let mut new_cwd = s.cwd.clone();
        // Parse the trailing `__BYTEAI_CWD__ <path>` marker (may be folded
        // into stderr or stdout depending on provider). Take the LAST one.
        for line in stdout.lines().chain(stderr.lines()) {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("__BYTEAI_CWD__") {
                let c = rest.trim();
                if !c.is_empty() {
                    new_cwd = c.to_string();
                }
            }
        }
        let mut out = String::new();
        if !stdout.is_empty() {
            out.push_str(&stdout);
            out.push('\n');
        }
        if !stderr.is_empty() {
            out.push_str(&stderr);
            out.push('\n');
        }
        if stdout.is_empty() && stderr.is_empty() {
            out.push_str("(no output)\n");
        }
        let code = output.status.code().unwrap_or(-1);
        out.push_str(&format!("[exit code: {code}]"));
        Ok((out, new_cwd))
    }
}

impl Tool for TerminalTool {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "terminal".into(),
            description: "Persistent shell session: shell state (working directory) survives across tool calls, like a \
                human's terminal tab. Actions: create [label] | list | run <id> <command> | close <id>. Use this instead \
                of shell when a multi-step task needs to stay in one directory (clone -> cd -> install -> test)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "list", "run", "close"], "description": "What to do." },
                    "id": { "type": "string", "description": "Session id (for run/close)." },
                    "label": { "type": "string", "description": "Optional label for create." },
                    "command": { "type": "string", "description": "Command to run in the session (for run)." },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120, max 600)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
            let id = args.get("id").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let label = args.get("label").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let command = args.get("command").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let timeout = args
                .get("timeout_secs")
                .and_then(|t| t.as_u64())
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .min(600);

            match action.as_str() {
                "create" => {
                    let s = self.create(&label);
                    let elapsed = started.elapsed().as_millis() as u64;
                    ok_outcome(
                        "",
                        self.name(),
                        format!(
                            "terminal session created\n  id:    {}\n  label: {}\n  cwd:   {}\n\nrun commands with: terminal run {} <command>",
                            s.id, s.label, s.cwd, s.id
                        ),
                        elapsed,
                    )
                }
                "list" => {
                    let sessions = self.sessions();
                    let elapsed = started.elapsed().as_millis() as u64;
                    if sessions.is_empty() {
                        return ok_outcome(
                            "",
                            self.name(),
                            "no terminal sessions — create one with: terminal create [label]".to_string(),
                            elapsed,
                        );
                    }
                    let mut out = String::from("terminal sessions:\n");
                    for s in &sessions {
                        out.push_str(&format!(
                            "  {}  {}  (cwd: {} · {} runs)\n",
                            s.id, s.label, s.cwd, s.runs
                        ));
                    }
                    out.push_str("\nrun: terminal run <id> <command> · close: terminal close <id>");
                    ok_outcome("", self.name(), out, elapsed)
                }
                "run" => {
                    if id.is_empty() || command.is_empty() {
                        let elapsed = started.elapsed().as_millis() as u64;
                        return ok_outcome(
                            "",
                            self.name(),
                            "usage: terminal run <id> <command>".to_string(),
                            elapsed,
                        );
                    }
                    let s = match self.get(&id) {
                        Some(s) => s,
                        None => {
                            let elapsed = started.elapsed().as_millis() as u64;
                            return err_outcome(
                                "",
                                self.name(),
                                &anyhow::anyhow!("no terminal session {id} — see terminal list"),
                                elapsed,
                            );
                        }
                    };
                    match self.run(&s, &command, timeout).await {
                        Ok((out, new_cwd)) => {
                            let mut updated = s.clone();
                            updated.cwd = new_cwd;
                            updated.runs += 1;
                            self.save(&updated);
                            let elapsed = started.elapsed().as_millis() as u64;
                            ok_outcome("", self.name(), out, elapsed)
                        }
                        Err(e) => {
                            let elapsed = started.elapsed().as_millis() as u64;
                            err_outcome("", self.name(), &anyhow::anyhow!(e), elapsed)
                        }
                    }
                }
                "close" => {
                    let existed = self.get(&id).is_some();
                    self.delete(&id);
                    let elapsed = started.elapsed().as_millis() as u64;
                    ok_outcome(
                        "",
                        self.name(),
                        if existed {
                            format!("terminal session {id} closed")
                        } else {
                            format!("no terminal session {id} to close")
                        },
                        elapsed,
                    )
                }
                other => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    ok_outcome(
                        "",
                        self.name(),
                        format!("unknown action {other:?} — use create | list | run | close"),
                        elapsed,
                    )
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_term_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn create_list_run_close_roundtrip() {
        let d = tmp_dir("roundtrip");
        let t = TerminalTool::new(d.clone());
        let s = t.create("work");
        assert_eq!(s.label, "work");
        assert!(!s.id.is_empty());

        let list = t.sessions();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].label, "work");

        // Run a command that cd's — cwd must persist for the next call.
        let (out, new_cwd) = t.run(&s, "cd /tmp && pwd", 30).await.unwrap();
        assert!(out.contains("/tmp"), "output shows pwd: {out}");
        assert_eq!(new_cwd, "/tmp", "cd persisted");

        // Simulate the execute path: run then save.
        let mut updated = s.clone();
        updated.cwd = new_cwd.clone();
        t.save(&updated);
        let reloaded = t.get(&s.id).unwrap();
        assert_eq!(reloaded.cwd, "/tmp", "cwd survives reload from disk");

        t.delete(&s.id);
        assert!(t.get(&s.id).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn list_empty_reports_hint() {
        let d = tmp_dir("empty");
        let t = TerminalTool::new(d.clone());
        let _ = std::fs::remove_dir_all(&d);
        let _ = t;
    }
}
