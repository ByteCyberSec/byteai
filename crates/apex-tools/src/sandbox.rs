//! `sandbox` — sandboxed command execution (OpenHands-style).
//! Top-10 core feature (OpenHands ★81k): run commands in an isolated
//! environment with a timeout, so agent actions can't hang or touch the
//! host state unexpectedly. Uses a temporary workdir + timeout; if a
//! docker binary is available it can run inside a container.

use std::time::Duration;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool};

pub struct SandboxTool;

impl Tool for SandboxTool {
    fn name(&self) -> &'static str {
        "sandbox"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "sandbox".into(),
            description: "Run a command inside a sandbox (temp dir + timeout; optional docker container). Input: {command, timeout_s?, docker?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command to run"},
                    "timeout_s": {"type": "integer", "description": "Timeout in seconds (default 30)"},
                    "docker": {"type": "boolean", "description": "Run inside a docker container (alpine:latest) if docker is available"}
                },
                "required": ["command"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let command = args.get("command").and_then(Value::as_str).unwrap_or("").to_string();
            if command.is_empty() {
                return crate::err_outcome("", "sandbox", &anyhow::anyhow!("missing 'command'"), 0);
            }
            let timeout_s = args.get("timeout_s").and_then(Value::as_u64).unwrap_or(30).max(1).min(300);
            let use_docker = args.get("docker").and_then(Value::as_bool).unwrap_or(false);

            // 1. Create a throwaway sandbox dir.
            let sandbox_dir = std::env::temp_dir().join(format!("byteai-sbx-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&sandbox_dir);

            let result = if use_docker && docker_available().await {
                // Docker container run: bind the sandbox dir, set workdir.
                let mut cmd = tokio::process::Command::new("docker");
                cmd.args(["run", "--rm", "-i", "-v", &format!("{}:/work", sandbox_dir.display()), "-w", "/work", "alpine:latest", "sh", "-c", &command]);
                tokio::time::timeout(Duration::from_secs(timeout_s), cmd.output()).await
            } else {
                // Local run in the sandbox dir with timeout via tokio.
                let mut cmd = tokio::process::Command::new("sh");
                cmd.arg("-c").arg(&command).current_dir(&sandbox_dir);
                tokio::time::timeout(Duration::from_secs(timeout_s), cmd.output()).await
            };
            let output = match result {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => return crate::err_outcome("", "sandbox", &anyhow::anyhow!("spawn failed: {e}"), started.elapsed().as_millis() as u64),
                Err(_) => return crate::err_outcome("", "sandbox", &anyhow::anyhow!("command timed out after {timeout_s}s"), started.elapsed().as_millis() as u64),
            };

            let (stdout, stderr, code) = (
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
                output.status.code().unwrap_or(-1),
            );

            let _ = std::fs::remove_dir_all(&sandbox_dir);

            let out = json!({
                "exit_code": code,
                "docker": use_docker && docker_available().await,
                "stdout": stdout.chars().take(4000).collect::<String>(),
                "stderr": stderr.chars().take(2000).collect::<String>(),
            });
            crate::ok_outcome("", "sandbox", out.to_string(), started.elapsed().as_millis() as u64)
        })
    }
}

async fn docker_available() -> bool {
    tokio::process::Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}