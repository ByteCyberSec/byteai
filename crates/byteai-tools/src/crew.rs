//! `crew` — role-based multi-agent crew with sequential handoff.
//! Top-10 core feature (CrewAI ★57k, MetaGPT ★70k): a task passes through
//! roles in order (e.g. architect -> engineer -> reviewer); each role gets a
//! real LLM call seeded with the prior role's output (handoff).

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ToolContext};

pub struct CrewTool {
    pub ctx: ToolContext,
}

impl CrewTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for CrewTool {
    fn name(&self) -> &'static str {
        "crew"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "crew".into(),
            description: "Role-based multi-agent crew: run a task through roles (architect/engineer/reviewer/…), each a real LLM call with handoff. Input: {task, roles?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "The task for the crew"},
                    "roles": {"type": "array", "items": {"type": "string", "enum": ["architect","engineer","reviewer","planner","tester"]}, "description": "Roles in order (default: architect, engineer, reviewer)"}
                },
                "required": ["task"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let task = args.get("task").and_then(Value::as_str).unwrap_or("").to_string();
            if task.is_empty() {
                return crate::err_outcome("", "crew", &anyhow::anyhow!("missing 'task'"), 0);
            }

            let roles: Vec<String> = args.get("roles")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| vec!["architect".into(), "engineer".into(), "reviewer".into()]);

            let role_prompts: std::collections::HashMap<&str, &str> = [
                ("architect", "You are the Architect. Design the high-level approach, modules, and data flow for the task. Be concrete and concise."),
                ("engineer", "You are the Engineer. Implement the task concretely: write the code/commands/plan in detail based on the architect's design."),
                ("reviewer", "You are the Reviewer. Review the work for correctness, bugs, security issues, and edge cases. List concrete findings and fixes."),
                ("planner", "You are the Planner. Break the task into ordered, executable steps with acceptance criteria."),
                ("tester", "You are the Tester. Define the test cases and verification steps that prove the task works."),
            ].iter().cloned().collect();

            let client = match &ctx.client {
                Some(c) => c.clone(),
                None => return crate::ok_outcome("", "crew", "No provider client available — crew requires a configured provider.", started.elapsed().as_millis() as u64),
            };
            let model = if ctx.default_model.is_empty() { "deepseek-v4-flash".to_string() } else { ctx.default_model.clone() };

            // Sequential handoff: each role's output seeds the next role.
            let mut handoff = String::new();
            let mut outputs = Vec::new();
            for role in &roles {
                let prompt = format!(
                    "{}\n\nTASK:\n{}\n\nPREVIOUS ROLE OUTPUT (handoff):\n{}\n\nRespond with your role's deliverable, concise and actionable.",
                    role_prompts.get(role.as_str()).unwrap_or(&"You are a team member on a crew."),
                    task,
                    if handoff.is_empty() { "(none — first role)" } else { &handoff }
                );
                let msg = byteai_types::Message::user(&prompt);
                match client.chat(&model, &[msg], &[], None).await {
                    Ok((text, _, _)) => {
                        handoff = text.clone();
                        outputs.push(json!({"role": role, "output": text.trim().chars().take(2000).collect::<String>()}));
                    }
                    Err(e) => {
                        outputs.push(json!({"role": role, "error": format!("{e:#}")}));
                    }
                }
            }

            let out = json!({
                "task": task,
                "roles": roles,
                "handoffs": outputs,
                "final_output": handoff.chars().take(2000).collect::<String>(),
            });
            crate::ok_outcome("", "crew", out.to_string(), started.elapsed().as_millis() as u64)
        })
    }
}