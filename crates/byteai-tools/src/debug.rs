//! Debug tool (DAP): launch, breakpoints, continue, stack, variables,
//! evaluate. Powered by the byteai-dap registry; degrades gracefully when no
//! adapter is available for the language.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use byteai_dap::{DapRegistry, DapState};
use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct DebugTool {
    dap: Option<Arc<DapRegistry>>,
}

impl DebugTool {
    pub fn new(dap: Option<Arc<DapRegistry>>) -> Self {
        Self { dap }
    }
}

impl Default for DebugTool {
    fn default() -> Self {
        Self::new(None)
    }
}

fn def() -> ToolDef {
    ToolDef {
        name: "debug".to_string(),
        description: "Debug a program via DAP. Actions: status (adapters available), \
launch <lang> <program> [args] [breakpoints], continue, next, step_in, step_out, \
stack, variables <frame_id>, evaluate <expr> <frame_id>, stop. Languages: python (debugpy). \
Sets breakpoints before launching; reports stack + locals when stopped.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["status", "launch", "continue", "next", "step_in", "step_out", "stack", "variables", "evaluate", "stop"] },
                "lang": { "type": "string", "description": "python, c, cpp, rust, node" },
                "program": { "type": "string" },
                "args": { "type": "array", "items": { "type": "string" } },
                "breakpoints": { "type": "array", "items": { "type": "integer" }, "description": "1-based line numbers" },
                "expr": { "type": "string" },
                "frame_id": { "type": "integer" }
            },
            "required": ["action"]
        }),
    }
}

impl Tool for DebugTool {
    fn name(&self) -> &'static str {
        "debug"
    }
    fn def(&self) -> ToolDef {
        def()
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let dap = self.dap.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("status");
            let Some(registry) = dap else {
                return ok_outcome("", "debug", "DAP disabled (no registry configured).", started.elapsed().as_millis() as u64);
            };

            if action == "status" {
                let langs = registry.available();
                return ok_outcome("", "debug", format!("DAP adapters available: {}", if langs.is_empty() { "none".into() } else { langs.join(", ") }), started.elapsed().as_millis() as u64);
            }

            let lang = args.get("lang").and_then(|l| l.as_str()).unwrap_or("python").to_string();
            let program = args.get("program").and_then(|p| p.as_str()).unwrap_or("").to_string();
            let lines: Vec<u64> = args.get("breakpoints").and_then(|b| b.as_array()).map(|a| a.iter().filter_map(|v| v.as_u64()).collect()).unwrap_or_default();
            let prog_args: Vec<String> = args.get("args").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();

            let cwd = PathBuf::from(".");
            let state = match registry.get(&lang, &cwd).await {
                Ok(s) => s,
                Err(e) => return ok_outcome("", "debug", format!("DAP unavailable: {e:#}"), started.elapsed().as_millis() as u64),
            };

            let mut st = state.lock().await;
            let session = match &mut *st {
                DapState::Ready(s) => s,
                DapState::Unavailable(reason) => return ok_outcome("", "debug", format!("DAP {lang} unavailable: {reason}"), started.elapsed().as_millis() as u64),
                DapState::Spawning => return ok_outcome("", "debug", "DAP still spawning".to_string(), started.elapsed().as_millis() as u64),
            };

            let result = match action {
                "launch" => {
                    if program.is_empty() {
                        "ERROR: `program` required for launch".to_string()
                    } else {
                        let cwd_s = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_else(|_| ".".into());
                        match session.launch(&program, prog_args, &cwd_s).await {
                            Ok(_) => {}
                            Err(e) => return ok_outcome("", "debug", format!("launch failed: {e:#}"), started.elapsed().as_millis() as u64),
                        }
                        match session.set_breakpoints(&program, &lines).await {
                            Ok(_) => {}
                            Err(e) => return ok_outcome("", "debug", format!("breakpoints failed: {e:#}"), started.elapsed().as_millis() as u64),
                        }
                        match session.configuration_done().await {
                            Ok(_) => {}
                            Err(e) => return ok_outcome("", "debug", format!("configurationDone failed: {e:#}"), started.elapsed().as_millis() as u64),
                        }
                        // Run until first stop (breakpoint or program end).
                        match session.wait_stopped(Duration::from_secs(10)).await {
                            Some(reason) => {
                                let mut out = format!("Launched; stopped: {reason}\n");
                                let threads = session.threads().await.unwrap_or_default();
                                if let Some(t) = threads.first()
                                    && let Ok(frames) = session.stack_trace(*t, 8).await {
                                        out.push_str(&format_stack(&frames));
                                    }
                                out
                            }
                            None => "Launched; program ran to completion (no stop event).".to_string(),
                        }
                    }
                }
                "continue" => {
                    let threads = session.threads().await.unwrap_or_default();
                    if threads.is_empty() {
                        "No threads (program not running?)".to_string()
                    } else {
                        match session.continue_run().await {
                            Ok(_) => match session.wait_stopped(Duration::from_secs(10)).await {
                                Some(reason) => {
                                    let mut out = format!("Continued; stopped: {reason}\n");
                                    let t = threads[0];
                                    if let Ok(frames) = session.stack_trace(t, 8).await {
                                        out.push_str(&format_stack(&frames));
                                    }
                                    out
                                }
                                None => "Continued; program ran to completion.".to_string(),
                            },
                            Err(e) => format!("continue failed: {e:#}"),
                        }
                    }
                }
                "next" | "step_in" | "step_out" => {
                    let threads = session.threads().await.unwrap_or_default();
                    let t = threads.first().copied().unwrap_or(1);
                    let r = match action {
                        "next" => session.next(t).await,
                        "step_in" => session.step_in(t).await,
                        _ => session.step_out(t).await,
                    };
                    match r {
                        Ok(_) => match session.wait_stopped(Duration::from_secs(10)).await {
                            Some(reason) => {
                                let mut out = format!("Stepped; stopped: {reason}\n");
                                if let Ok(frames) = session.stack_trace(t, 6).await {
                                    out.push_str(&format_stack(&frames));
                                }
                                out
                            }
                            None => "Stepped; program finished.".to_string(),
                        },
                        Err(e) => format!("step failed: {e:#}"),
                    }
                }
                "stack" => {
                    let threads = session.threads().await.unwrap_or_default();
                    let t = threads.first().copied().unwrap_or(1);
                    match session.stack_trace(t, 12).await {
                        Ok(frames) => format_stack(&frames),
                        Err(e) => format!("stackTrace failed: {e:#}"),
                    }
                }
                "variables" => {
                    let frame_id = args.get("frame_id").and_then(|f| f.as_u64()).unwrap_or(0);
                    if frame_id == 0 {
                        "ERROR: `frame_id` required (get one from `stack`)".to_string()
                    } else {
                        match session.scopes(frame_id).await {
                            Ok(scopes) => {
                                let mut out = String::new();
                                for sc in scopes.iter().take(4) {
                                    let name = sc.get("name").and_then(|n| n.as_str()).unwrap_or("scope");
                                    let vr = sc.get("variablesReference").and_then(|v| v.as_u64()).unwrap_or(0);
                                    out.push_str(&format!("[{name}]\n"));
                                    if vr > 0
                                        && let Ok(vars) = session.variables(vr).await {
                                            for v in vars.iter().take(30) {
                                                let vn = v.get("name").and_then(|x| x.as_str()).unwrap_or("?");
                                                let vv = v.get("value").and_then(|x| x.as_str()).unwrap_or("?");
                                                out.push_str(&format!("  {vn} = {vv}\n"));
                                            }
                                        }
                                }
                                if out.is_empty() { "No scopes".into() } else { out }
                            }
                            Err(e) => format!("scopes failed: {e:#}"),
                        }
                    }
                }
                "evaluate" => {
                    let expr = args.get("expr").and_then(|e| e.as_str()).unwrap_or("").to_string();
                    let frame_id = args.get("frame_id").and_then(|f| f.as_u64()).unwrap_or(0);
                    match session.evaluate(&expr, frame_id).await {
                        Ok(r) => format!("{expr} = {r}"),
                        Err(e) => format!("evaluate failed: {e:#}"),
                    }
                }
                "stop" => {
                    match session.disconnect().await {
                        Ok(_) => "Debug session stopped.".to_string(),
                        Err(e) => format!("disconnect failed: {e:#}"),
                    }
                }
                other => format!("ERROR: unknown action {other:?}"),
            };
            ok_outcome("", "debug", result, started.elapsed().as_millis() as u64)
        })
    }
}

fn format_stack(frames: &[byteai_dap::StackFrame]) -> String {
    let mut out = String::new();
    for (i, f) in frames.iter().enumerate() {
        let src = f.source.split('/').next_back().unwrap_or(&f.source);
        out.push_str(&format!("  #{i} {:<28} {}:{}\n", f.name, src, f.line));
    }
    if frames.is_empty() {
        out.push_str("  (empty stack)\n");
    }
    out
}
