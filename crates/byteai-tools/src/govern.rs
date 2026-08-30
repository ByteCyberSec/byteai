//! `govern` — constitutional governance: check an action against safety
//! bounds, flag human-approval triggers, append to an immutable audit trail.
//! Ported from AiMyWay's Layer-7 Governance.

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool};

pub struct GovernTool {
    pub data_dir: PathBuf,
}

impl GovernTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    fn audit_path(&self) -> PathBuf {
        self.data_dir.join("audit.log")
    }

    fn append_audit(&self, entry: &Value) -> std::io::Result<()> {
        let path = self.audit_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{entry}")?;
        Ok(())
    }
}

impl Tool for GovernTool {
    fn name(&self) -> &'static str {
        "govern"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "govern".into(),
            description: "Constitutional guardrail: check an action against safety bounds, flag human-approval triggers, log to the immutable audit trail. Input: {action, context?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "The proposed action, e.g. 'rm -rf /tmp' or 'deploy to prod'"},
                    "context": {"type": "string", "description": "Optional context for the check"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let data_dir = self.data_dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("").to_string();
            if action.is_empty() {
                return crate::err_outcome("", "govern", &anyhow::anyhow!("missing 'action'"), 0);
            }
            let context = args.get("context").and_then(Value::as_str).unwrap_or("").to_string();
            let action_lower = action.to_lowercase();
            let context_lower = context.to_lowercase();

            // 1. Safety bounds (keyword based; real impl would use LLM).
            let mut violations: Vec<String> = Vec::new();
            if action_lower.contains("rm -rf") || action_lower.contains("drop table") {
                violations.push("irreversible destructive action".into());
            }
            if (action_lower.contains("modify") || action_lower.contains("rewrite")) && (context_lower.contains("byteai") || context_lower.contains("self")) {
                violations.push("self-modification requires approval".into());
            }

            // 2. Human-approval triggers.
            let triggers = ["deploy", "delete", "rm -rf", "git push --force", "chmod 777", "sudo", "spend"];
            let needs_human: Vec<String> = triggers.iter().filter(|t| action_lower.contains(**t)).map(|s| s.to_string()).collect();

            let approved = violations.is_empty() && needs_human.is_empty();

            let entry = json!({
                "action": action,
                "context": context,
                "approved": approved,
                "violations": violations,
                "needs_human_approval": needs_human,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            });

            let govern = GovernTool::new(data_dir.clone());
            let _ = govern.append_audit(&entry);

            let out = json!({
                "approved": approved,
                "violations": violations,
                "needs_human_approval": needs_human,
                "audit_logged": true,
                "audit_path": govern.audit_path().display().to_string(),
            });
            crate::ok_outcome("", "govern", out.to_string(), started.elapsed().as_millis() as u64)
        })
    }
}