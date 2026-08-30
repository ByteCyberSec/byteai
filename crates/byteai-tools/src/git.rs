//! `git` — Aider-style git workflow for agentic coding.
//! Top-10 core feature (Aider ★44k): status/diff/add/commit/log so the
//! agent can pair with git and auto-commit its own work.

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool};

pub struct GitTool;

impl Tool for GitTool {
    fn name(&self) -> &'static str {
        "git"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "git".into(),
            description: "Run a git workflow: status, diff, add, commit, log. Input: {action, files?, message?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["status","diff","add","commit","log","commit_all"], "description": "Git operation"},
                    "files": {"type": "array", "items": {"type": "string"}, "description": "Files for add (default: all changes)"},
                    "message": {"type": "string", "description": "Commit message (for commit/commit_all)"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("").to_string();
            if action.is_empty() {
                return crate::err_outcome("", "git", &anyhow::anyhow!("missing 'action'"), 0);
            }

            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let out = match action.as_str() {
                "status" => run_git(&cwd, &["status", "--short", "--branch"]).await,
                "diff" => run_git(&cwd, &["diff", "--stat"]).await,
                "log" => run_git(&cwd, &["log", "--oneline", "-10"]).await,
                "add" => {
                    let files: Vec<String> = args.get("files")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    if files.is_empty() {
                        run_git(&cwd, &["add", "-A"]).await
                    } else {
                        let mut cmd = vec!["add".to_string()];
                        cmd.extend(files);
                        run_git(&cwd, &cmd.iter().map(|s| s.as_str()).collect::<Vec<_>>()).await
                    }
                }
                "commit" => {
                    let msg = args.get("message").and_then(Value::as_str).unwrap_or("auto-commit by ByteAi").to_string();
                    run_git(&cwd, &["commit", "-m", &msg]).await
                }
                "commit_all" => {
                    let msg = args.get("message").and_then(Value::as_str).unwrap_or("auto-commit by ByteAi").to_string();
                    let s = run_git(&cwd, &["add", "-A"]).await;
                    if s.starts_with("fatal") || s.starts_with("error") {
                        s
                    } else {
                        run_git(&cwd, &["commit", "-m", &msg]).await
                    }
                }
                other => format!("unknown action '{other}' — use status/diff/add/commit/log/commit_all"),
            };
            crate::ok_outcome("", "git", out, started.elapsed().as_millis() as u64)
        })
    }
}

async fn run_git(cwd: &PathBuf, args: &[&str]) -> String {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(cwd).output().await.map(|o| {
        let mut s = String::from_utf8_lossy(&o.stdout).to_string();
        if !o.stderr.is_empty() {
            s.push_str(&String::from_utf8_lossy(&o.stderr));
        }
        s.trim().to_string()
    }).unwrap_or_else(|e| format!("git failed: {e:#}"))
}