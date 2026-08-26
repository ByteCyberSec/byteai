//! Shell tool: run a command with timeout, capture stdout/stderr/exit code.

use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, err_outcome, ok_outcome};

pub const OUTPUT_CAP: usize = 96 * 1024; // per-stream cap (96 KiB)
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[derive(Default)]
pub struct ShellTool;

impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "shell".into(),
            description: "Run a shell command in the working directory. Returns stdout, stderr, exit code, and elapsed time. "
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to run (bash syntax)." },
                    "cwd": { "type": "string", "description": "Working directory (default: session cwd)." },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default 120, max 600)." }
                },
                "required": ["command"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let command = match args.get("command").and_then(|c| c.as_str()) {
                Some(c) => c.to_string(),
                None => {
                    return ok_outcome("", self.name(), "ERROR: missing required argument `command`", 0);
                }
            };
            let cwd = args.get("cwd").and_then(|c| c.as_str()).map(String::from);
            let timeout = args
                .get("timeout_secs")
                .and_then(|t| t.as_u64())
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .min(600);

            let mut cmd = tokio::process::Command::new("/bin/bash");
            cmd.arg("-c").arg(&command);
            if let Some(c) = &cwd {
                cmd.current_dir(c);
            }
            cmd.kill_on_drop(true);
            let output = match cmd.output().await {
                Ok(o) => o,
                Err(e) => {
                    let elapsed = started.elapsed().as_millis() as u64;
                    return err_outcome("", self.name(), &anyhow::anyhow!("spawn failed: {e}"), elapsed);
                }
            };
            let timed_out = output.status.code().is_none();
            let elapsed = started.elapsed().as_millis() as u64;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut out = String::new();
            if timed_out {
                out.push_str(&format!("[TIMEOUT after {timeout}s — process killed]\n"));
            }
            if !stdout.is_empty() {
                out.push_str(&format!("--- stdout ({} bytes) ---\n", stdout.len()));
                out.push_str(&cap(&stdout, OUTPUT_CAP));
                out.push('\n');
            }
            if !stderr.is_empty() {
                out.push_str(&format!("--- stderr ({} bytes) ---\n", stderr.len()));
                out.push_str(&cap(&stderr, OUTPUT_CAP));
                out.push('\n');
            }
            if stdout.is_empty() && stderr.is_empty() {
                out.push_str("(no output)\n");
            }
            let code = output.status.code().unwrap_or(-1);
            out.push_str(&format!("[exit code: {code}, elapsed: {elapsed} ms]"));
            let _ = &code;
            ok_outcome("", self.name(), out, elapsed)
        })
    }
}

pub fn cap(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t = s[..max].to_string();
        t.push_str(&format!("\n… [truncated {} bytes]", s.len() - max));
        t
    }
}
