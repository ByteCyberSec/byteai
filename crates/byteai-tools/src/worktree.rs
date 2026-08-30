//! `worktree` — manage git worktrees (create/list/remove).
//!
//! Wraps `git worktree` for parallel feature work without stash juggling.
//! Actions: create, list, remove.

use std::time::Instant;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct WorktreeTool;

impl Tool for WorktreeTool {
    fn name(&self) -> &'static str {
        "worktree"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "worktree".into(),
            description: "Manage git worktrees for parallel feature work: create a linked working tree, list them, or remove one. Input: {action: create|list|remove, path?, branch?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "list", "remove"]},
                    "path": {"type": "string", "description": "worktree path (create/remove)"},
                    "branch": {"type": "string", "description": "new branch name for create"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("list").to_string();
            let path = args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let branch = args.get("branch").and_then(Value::as_str).unwrap_or("").to_string();
            let out = match action.as_str() {
                "list" => cmd("git", ["worktree", "list"]).await,
                "create" if path.is_empty() => "usage: worktree create <path> [branch]".into(),
                "create" => {
                    let mut args = vec!["worktree", "add", "-b", &branch, &path];
                    if branch.is_empty() {
                        args = vec!["worktree", "add", &path];
                    }
                    cmd("git", &args).await
                }
                "remove" if path.is_empty() => "usage: worktree remove <path>".into(),
                "remove" => cmd("git", &["worktree", "remove", &path]).await,
                other => format!("unknown action {other:?} — use create|list|remove"),
            };
            ok_outcome("", "worktree", out, started.elapsed().as_millis() as u64)
        })
    }
}

async fn cmd(prog: &str, args: impl AsRef<[&str]>) -> String {
    match Command::new(prog).args(args.as_ref()).output().await {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => format!("failed ({}):\n{}", o.status, String::from_utf8_lossy(&o.stderr)),
        Err(e) => format!("{prog} not available: {e:#}"),
    }
}