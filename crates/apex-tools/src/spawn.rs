//! Spawn tool (Phase 7 — multi-agent). Run N parallel `byteai chat` sub-processes
//! with isolated prompts, collect results, bounded concurrency.

use std::sync::Arc;
use std::time::Duration;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{BoxFuture, Tool, ok_outcome};

const AGENT_TIMEOUT: Duration = Duration::from_secs(300);

pub struct SpawnTool;

impl Tool for SpawnTool {
    fn name(&self) -> &'static str {
        "spawn"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "spawn".into(),
            description: "Spawn 1-5 parallel sub-agents, each a `byteai chat` process with \
an isolated prompt. Collects their outputs. Bounded concurrency (max 5). \
Use for parallel research, independent code generation, or multi-perspective review.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "goals": { "type": "array", "items": { "type": "string" }, "description": "1-5 goals, one per sub-agent" },
                    "max_parallel": { "type": "integer", "description": "Max concurrent workers (default 3)" }
                },
                "required": ["goals"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let goals: Vec<String> = args
                .get("goals")
                .and_then(|g| g.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if goals.is_empty() {
                return ok_outcome("", "spawn", "ERROR: `goals` array (1-5 strings) required".to_string(), started.elapsed().as_millis() as u64);
            }
            let max_parallel = args.get("max_parallel").and_then(|m| m.as_u64()).unwrap_or(3).min(5).max(1) as usize;

            // Locate byteai binary.
            let byteai = std::env::current_exe().ok().unwrap_or_else(|| "byteai".into());

            let semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel));
            let mut handles = Vec::new();

            for (i, goal) in goals.iter().enumerate() {
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let cmd = byteai.clone();
                let g = goal.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = permit;
                    let output = timeout(AGENT_TIMEOUT, Command::new(&cmd)
                        .args(["chat", "--no-save", "--model", "b.ai", &g])
                        .output())
                    .await;
                    (i, g, output)
                }));
            }

            let mut results: Vec<(usize, String, String)> = Vec::new();
            for h in handles {
                match h.await {
                    Ok((i, g, Ok(Ok(out)))) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let combined = if out.status.success() {
                            stdout.to_string()
                        } else {
                            format!("exit {}:\n{}\n{}", out.status.code().unwrap_or(-1), stdout, stderr.lines().rev().take(6).collect::<Vec<_>>().join("\n"))
                        };
                        results.push((i, g, combined));
                    }
                    Ok((i, g, Ok(Err(e)))) => { results.push((i, g, format!("IO error: {e}"))); }
                    Ok((i, g, Err(_))) => { results.push((i, g, "timed out".into())); }
                    Err(e) => { results.push((0, String::new(), format!("join error: {e}"))); }
                }
            }

            results.sort_by_key(|r| r.0);
            let mut out = String::new();
            out.push_str(&format!("spawned {} sub-agent(s), {} parallel\n", goals.len(), max_parallel));
            for (i, goal, output) in &results {
                let preview: String = output.chars().take(240).collect();
                out.push_str(&format!("\n--- Agent {i} ---\nGoal: {goal}\nOutput: {preview}\n"));
            }
            ok_outcome("", "spawn", out, started.elapsed().as_millis() as u64)
        })
    }
}