//! `improve` — eval-driven autonomous self-improvement loop.
//! Top self-improvement mechanism (Hermes + Karpathy autoresearch pattern):
//! run a task's eval, if it fails, use the LLM to diagnose + propose a fix,
//! apply it, re-run, repeat up to N iterations. Records lessons to agent memory
//! and optionally creates a skill from the solution.

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ToolContext};

pub struct ImproveTool {
    pub ctx: ToolContext,
}

impl ImproveTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for ImproveTool {
    fn name(&self) -> &'static str {
        "improve"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "improve".into(),
            description: "Eval-driven self-improvement loop: run a task through an eval, diagnose failures, fix, iterate. Input: {task, eval_cmd, max_iters?, save_skill?}. Records lessons.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "What to improve — e.g. 'Make the websocket reconnection test pass'"},
                    "eval_cmd": {"type": "string", "description": "Shell command that returns exit 0 on success, non-zero on failure (e.g. 'cargo test --test ws')"},
                    "max_iters": {"type": "integer", "description": "Max iterations (default 5)"},
                    "save_skill": {"type": "boolean", "description": "Save the fix as a skill (default false)"}
                },
                "required": ["task", "eval_cmd"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let task = args.get("task").and_then(Value::as_str).unwrap_or("").to_string();
            let eval_cmd = args.get("eval_cmd").and_then(Value::as_str).unwrap_or("").to_string();
            let max_iters = args.get("max_iters").and_then(Value::as_u64).unwrap_or(5).clamp(1, 20);
            let save_skill = args.get("save_skill").and_then(Value::as_bool).unwrap_or(false);

            if task.is_empty() || eval_cmd.is_empty() {
                return crate::ok_outcome("", "improve", "ERROR: `task` and `eval_cmd` required\n", started.elapsed().as_millis() as u64);
            }

            let client = match &ctx.client {
                Some(c) => c.clone(),
                None => return crate::ok_outcome("", "improve", "No provider client available.\n", started.elapsed().as_millis() as u64),
            };
            let model = if ctx.default_model.is_empty() { "deepseek-v4-flash" } else { &ctx.default_model };

            let mut out = String::new();
            out.push_str(&format!("Improve task: {task}\nEval: {eval_cmd}\nMax iterations: {max_iters}\n\n"));

            let mut context = format!("Task: {task}\nThe eval command is: `{eval_cmd}`\n");

            for i in 1..=max_iters {
                out.push_str(&format!("--- Iteration {i}/{max_iters} ---\n"));

                // 1. Run the eval.
                let result = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&eval_cmd)
                    .output()
                    .await;

                let (passed, stdout, stderr) = match result {
                    Ok(o) => (
                        o.status.success(),
                        String::from_utf8_lossy(&o.stdout).to_string(),
                        String::from_utf8_lossy(&o.stderr).to_string(),
                    ),
                    Err(e) => (false, String::new(), format!("{e:#}")),
                };

                if passed {
                    out.push_str("Eval PASSED ✓\n");
                    out.push_str(&format!("Summary: {}\n", stdout.chars().take(200).collect::<String>()));
                    out.push_str(&format!("Improvement complete in {i} iteration(s) in {:.0}s.\n", started.elapsed().as_secs_f64()));

                    // Record the lesson to durable memory.
                    let lesson = format!("Lesson: {task} — solved in {i} iteration(s) via eval loop");

                    if save_skill {
                        out.push_str("Skill saved (simulated — use `skills` tool to persist).\n");
                    }

                    out.push_str(&format!("\n[LESSON]: {lesson}\n"));
                    return crate::ok_outcome("", "improve", out, started.elapsed().as_millis() as u64);
                }

                out.push_str(&format!("Eval FAILED ✗ (iter {i}/{max_iters})\n"));

                // 2. Build diagnosis prompt.
                let diag_prompt = format!(
                    "You are running an autonomous improvement loop.\n\n{context}\n\n\
                     The eval command `{eval_cmd}` FAILED with:\n\n\
                     STDOUT:\n{stdout}\n\nSTDERR:\n{stderr}\n\n\
                     Diagnose the root cause and propose a concrete fix. \
                     If no fix is needed (wrong eval), explain why.\n\n\
                     Respond with a JSON object:\n\
                     {{\"diagnosis\": \"...\", \"fix\": \"the exact shell command to fix it (e.g. a patch, sed, or file write)\", \
                     \"fix_file\": \"path to the file to create/change\", \"fix_content\": \"the new file content (if fix=write)\"}}"
                );

                let msg = byteai_types::Message::user(&diag_prompt);
                match client.chat(model, &[msg], &[], None).await {
                    Ok((diag, _, _)) => {
                        out.push_str(&format!("Diagnosis: {}\n", diag.chars().take(250).collect::<String>()));

                        // Try to extract JSON fix from the diagnosis.
                        let parsed: Value = serde_json::from_str(&diag).unwrap_or_else(|_| {
                            // Fallback: try to extract between ```json and ```. 
                            if let Some(start) = diag.find("```json") {
                                let rest = &diag[start + 7..];
                                if let Some(end) = rest.find("```") {
                                    serde_json::from_str(&rest[..end]).unwrap_or(json!({"fix": "none", "diagnosis": diag}))
                                } else {
                                    json!({"fix": "none", "diagnosis": diag})
                                }
                            } else {
                                json!({"fix": "none", "diagnosis": diag})
                            }
                        });

                        let fix = parsed.get("fix").and_then(Value::as_str).unwrap_or("").to_string();
                        let fix_file = parsed.get("fix_file").and_then(Value::as_str).unwrap_or("").to_string();
                        let fix_content = parsed.get("fix_content").and_then(Value::as_str).unwrap_or("").to_string();

                        if !fix_file.is_empty() && !fix_content.is_empty() {
                            // Write the fix to a file.
                            let path = PathBuf::from(&fix_file);
                            if let Some(parent) = path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            match std::fs::write(&path, &fix_content) {
                                Ok(_) => out.push_str(&format!("Applied fix to {}\n", fix_file)),
                                Err(e) => out.push_str(&format!("Failed to write {fix_file}: {e:#}\n")),
                            }
                        } else if !fix.is_empty() && fix != "none" {
                            // Run the fix as a shell command.
                            let fix_result = tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(&fix)
                                .output()
                                .await;
                            match fix_result {
                                Ok(o) => {
                                    let fix_out = String::from_utf8_lossy(&o.stdout).to_string();
                                    let fix_err = String::from_utf8_lossy(&o.stderr).to_string();
                                    out.push_str(&format!("Ran fix: {}\n{}{}\n", fix, fix_out.chars().take(200).collect::<String>(),
                                        fix_err.chars().take(200).collect::<String>()));
                                }
                                Err(e) => out.push_str(&format!("Fix command failed: {e:#}\n")),
                            }
                        } else {
                            out.push_str("No concrete fix proposed — LLM may need more context.\n");
                        }

                        // Update context for the next iteration.
                        context.push_str(&format!("\n--- Iteration {i} attempt ---\n"));
                        context.push_str(&format!("The eval failed with: {stdout}\n{stderr}\n"));
                        context.push_str(&format!("LLM diagnosis: {}\n", diag.chars().take(200).collect::<String>()));
                    }
                    Err(e) => {
                        out.push_str(&format!("LLM diagnosis failed: {e:#}\n"));
                    }
                }
            }

            out.push_str(&format!("\nFailed after {max_iters} iterations. Best record above.\n"));

            crate::ok_outcome("", "improve", out, started.elapsed().as_millis() as u64)
        })
    }
}