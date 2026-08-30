//! `proc` — background process management (Hermes terminal background parity).
//!
//! Start a long-running command detached from the agent (survives tool
//! timeouts and parent exit), then poll its status, tail its log, and kill
//! it when done. Without this, a build/test/daemon that outlives the tool
//! timeout either hangs the turn or is killed outright.
//!
//! Implementation: spawn via `nohup <cmd> > <log> 2>&1 & echo $!` so the
//! child detaches (setsid-equivalent, works on macOS + Linux without a
//! dependency), capture the PID, and track it in a JSON registry under
//! `<data>/procs/registry.json`. Logs land in `<data>/procs/<id>.log`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

/// A tracked background process.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcEntry {
    pub id: String,
    pub name: String,
    pub command: String,
    pub pid: u32,
    pub log_path: String,
    pub started_ms: u64,
}

pub struct ProcTool {
    registry_path: Mutex<PathBuf>,
    logs_dir: Mutex<PathBuf>,
}

impl ProcTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let procs_dir = data_dir.join("procs");
        let _ = std::fs::create_dir_all(&procs_dir);
        Self {
            registry_path: Mutex::new(procs_dir.join("registry.json")),
            logs_dir: Mutex::new(procs_dir),
        }
    }

    fn load(&self) -> Vec<ProcEntry> {
        let p = self.registry_path.lock().unwrap();
        std::fs::read_to_string(&*p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, procs: &[ProcEntry]) {
        let p = self.registry_path.lock().unwrap();
        let _ = std::fs::write(&*p, serde_json::to_string_pretty(procs).unwrap_or_default());
    }

    fn is_alive(&self, pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Tool for ProcTool {
    fn name(&self) -> &'static str {
        "proc"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "proc".into(),
            description: "Background process manager. Actions: \
start {command, name?} — run a long command detached (survives timeouts); returns an id. \
status {id} — is it running, elapsed, exit status. \
log {id, tail?} — read the last N lines (default 200). \
kill {id} — terminate. \
list — all tracked processes. \
Use for builds, test suites, servers, and anything that outlives the tool timeout."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["start","status","log","kill","list"]},
                    "command": {"type": "string", "description": "Shell command to run in background (for start)"},
                    "name": {"type": "string", "description": "Optional friendly name"},
                    "id": {"type": "string", "description": "Process id"},
                    "tail": {"type": "integer", "description": "Lines from the end of the log (default 200)"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let reg = self.registry_path.lock().unwrap().clone();
        let logs = self.logs_dir.lock().unwrap().clone();
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
            let command = args.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("proc").to_string();
            let id = args.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let tail = args.get("tail").and_then(|t| t.as_u64()).unwrap_or(200) as usize;

            // Reconstruct the tool for mutable ops.
            let tool = ProcTool {
                registry_path: Mutex::new(reg),
                logs_dir: Mutex::new(logs),
            };

            let out = match action.as_str() {
                "start" => {
                    if command.is_empty() {
                        "ERROR: `command` required".to_string()
                    } else {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let proc_id = format!("proc-{}", now);
                        let log_path = tool.logs_dir.lock().unwrap().join(format!("{proc_id}.log"));
                        let log_str = log_path.to_string_lossy().to_string();
                        // Write the command to a temp script so `nohup` applies
                        // to the WHOLE command (a bare `nohup cmd1; cmd2 &`
                        // would only nohup cmd1 and the rest dies on SIGHUP).
                        let script_path = tool.logs_dir.lock().unwrap().join(format!("{proc_id}.sh"));
                        let script_str = shell_quote(&script_path.to_string_lossy());
                        let write_ok = std::fs::write(&script_path, format!("#!/bin/bash\n{command}\n"));
                        if write_ok.is_err() {
                            format!("ERROR: could not write launcher script: {}", write_ok.unwrap_err())
                        } else {
                            // nohup + background + echo PID: detaches the child so
                            // it survives this tool call (and the parent process).
                            // Paths are shell-quoted because the data dir may
                            // contain spaces (e.g. "Application Support").
                            let log_quoted = shell_quote(&log_path.to_string_lossy());
                            let spawn_cmd = format!(
                                "nohup bash {script_str} > {log_quoted} 2>&1 & echo $!"
                            );
                            let output = std::process::Command::new("bash")
                                .arg("-c")
                                .arg(&spawn_cmd)
                                .output();
                            match output {
                                Ok(o) => {
                                    let pid_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                    let pid: u32 = pid_str.parse().unwrap_or(0);
                                    if pid == 0 {
                                        format!("ERROR: could not start command (no PID returned). stderr: {}", String::from_utf8_lossy(&o.stderr))
                                    } else {
                                        let mut procs = tool.load();
                                        let name_clone = name.clone();
                                        procs.push(ProcEntry {
                                            id: proc_id.clone(),
                                            name,
                                            command,
                                            pid,
                                            log_path: log_str,
                                            started_ms: now,
                                        });
                                        tool.save(&procs);
                                        format!("started {proc_id} pid={pid} name={name_clone}\nlog: {}", log_path.display())
                                    }
                                }
                                Err(e) => format!("ERROR: spawn failed: {e}"),
                            }
                        }
                    }
                }
                "status" => {
                    let procs = tool.load();
                    match procs.iter().find(|p| p.id == id) {
                        Some(p) => {
                            let alive = tool.is_alive(p.pid);
                            let elapsed_s = now_secs().saturating_sub(p.started_ms / 1000);
                            format!(
                                "{}\tname={}\tpid={}\trunning={}\telapsed={}s\tlog={}",
                                p.id, p.name, p.pid, alive, elapsed_s, p.log_path
                            )
                        }
                        None => format!("ERROR: no process {id}"),
                    }
                }
                "log" => {
                    let procs = tool.load();
                    match procs.iter().find(|p| p.id == id) {
                        Some(p) => {
                            match std::fs::read_to_string(&p.log_path) {
                                Ok(text) => {
                                    let lines: Vec<&str> = text.lines().collect();
                                    let from = lines.len().saturating_sub(tail);
                                    let slice: Vec<&str> = lines[from..].to_vec();
                                    if slice.is_empty() {
                                        "(log is empty so far)".to_string()
                                    } else {
                                        slice.join("\n")
                                    }
                                }
                                Err(e) => format!("ERROR: log unreadable: {e}"),
                            }
                        }
                        None => format!("ERROR: no process {id}"),
                    }
                }
                "kill" => {
                    let mut procs = tool.load();
                    match procs.iter().find(|p| p.id == id).cloned() {
                        Some(p) => {
                            let _ = std::process::Command::new("kill")
                                .arg(p.pid.to_string())
                                .status();
                            // Give it a moment to die, then SIGKILL if needed.
                            std::thread::sleep(Duration::from_millis(300));
                            if tool.is_alive(p.pid) {
                                let _ = std::process::Command::new("kill")
                                    .arg("-9")
                                    .arg(p.pid.to_string())
                                    .status();
                            }
                            procs.retain(|x| x.id != id);
                            tool.save(&procs);
                            format!("killed {} (pid {})", p.id, p.pid)
                        }
                        None => format!("ERROR: no process {id}"),
                    }
                }
                "list" => {
                    let procs = tool.load();
                    if procs.is_empty() {
                        "no background processes".to_string()
                    } else {
                        let mut out = String::from("id\tname\tpid\trunning\telapsed_s\n");
                        for p in &procs {
                            let alive = tool.is_alive(p.pid);
                            let elapsed_s = now_secs().saturating_sub(p.started_ms / 1000);
                            out.push_str(&format!("{}\t{}\t{}\t{}\t{}\n", p.id, p.name, p.pid, alive, elapsed_s));
                        }
                        out
                    }
                }
                other => format!("ERROR: unknown action {other:?}"),
            };
            ok_outcome("", "proc", out, started.elapsed().as_millis() as u64)
        })
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Quote a path for safe interpolation into a shell command (spaces, quotes).
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

    fn tmp_proc(tag: &str) -> (PathBuf, ProcTool) {
        let d = std::env::temp_dir().join(format!("byteai_proc_test_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (d.clone(), ProcTool::new(d))
    }

    #[tokio::test]
    async fn start_status_log_kill_roundtrip() {
        let (_d, tool) = tmp_proc("roundtrip");
        // A long-running command that writes progress then sleeps.
        let cmd = "i=0; while [ $i -lt 100 ]; do echo \"line-$i\"; i=$((i+1)); sleep 0.05; done";
        let out = tool.execute(json!({"action":"start","command":cmd,"name":"writer"})).await.output;
        assert!(out.starts_with("started proc-"), "start: {out}");
        let id = out.split_whitespace().nth(1).unwrap().to_string();

        // Give it a moment to write some lines.
        std::thread::sleep(Duration::from_millis(200));
        let status = tool.execute(json!({"action":"status","id":id})).await.output;
        assert!(status.contains("running=true"), "status: {status}");

        let log = tool.execute(json!({"action":"log","id":id,"tail":100})).await.output;
        assert!(log.contains("line-"), "log should contain output: {log}");

        let killed = tool.execute(json!({"action":"kill","id":id})).await.output;
        assert!(killed.contains("killed"), "kill: {killed}");
        // After kill, it should be gone from the registry.
        let list = tool.execute(json!({"action":"list"})).await.output;
        assert!(!list.contains(&id), "killed proc removed: {list}");
    }

    #[tokio::test]
    async fn start_rejects_missing_command() {
        let (_d, tool) = tmp_proc("nocmd");
        let out = tool.execute(json!({"action":"start"})).await.output;
        assert!(out.contains("`command` required"), "{out}");
    }

    #[tokio::test]
    async fn unknown_id_errors_cleanly() {
        let (_d, tool) = tmp_proc("badid");
        let out = tool.execute(json!({"action":"status","id":"nope"})).await.output;
        assert!(out.contains("no process nope"), "{out}");
        let out = tool.execute(json!({"action":"kill","id":"nope"})).await.output;
        assert!(out.contains("no process nope"), "{out}");
    }

    #[test]
    fn shell_quote_handles_spaces() {
        // Path with spaces must be single-quoted.
        let q = shell_quote("/Users/me/Library/Application Support/byteai/procs/x.log");
        assert_eq!(q, "'/Users/me/Library/Application Support/byteai/procs/x.log'");
        // Simple safe paths pass through unchanged.
        assert_eq!(shell_quote("/tmp/x.sh"), "/tmp/x.sh");
        // Embedded single quote is escaped.
        let q2 = shell_quote("/tmp/it's.sh");
        assert_eq!(q2, "'/tmp/it'\\''s.sh'");
        // Empty string becomes a quoted empty arg.
        assert_eq!(shell_quote(""), "''");
    }

    #[tokio::test]
    async fn start_works_with_spaces_in_path() {
        // Data dir with a space in the name — regression test for the
        // unquoted redirect bug.
        let base = std::env::temp_dir()
            .join(format!("byteai proc test {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let tool = ProcTool::new(base.clone());
        let out = tool.execute(json!({"action":"start","command":"echo spaced-path-ok","name":"sp"})).await.output;
        assert!(out.starts_with("started proc-"), "start: {out}");
        let id = out.split_whitespace().nth(1).unwrap().to_string();
        std::thread::sleep(Duration::from_millis(200));
        let log = tool.execute(json!({"action":"log","id":id})).await.output;
        assert!(log.contains("spaced-path-ok"), "log should have output: {log}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
