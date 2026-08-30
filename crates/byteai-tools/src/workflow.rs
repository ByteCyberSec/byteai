//! `workflow` — persistent multi-step workflow with state (LangGraph / deer-flow
//! pattern). Define named workflows of steps; run them with state carried
//! between steps; workflow + state persist to disk so long-horizon runs
//! survive restarts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Step {
    name: String,
    instruction: String,
    #[serde(default)]
    output: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Workflow {
    id: String,
    name: String,
    steps: Vec<Step>,
    cursor: usize,
    #[serde(default)]
    state: HashMap<String, String>,
    done: bool,
}

pub struct WorkflowTool {
    path: Mutex<PathBuf>,
}

impl WorkflowTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { path: Mutex::new(data_dir.join("workflows.json")) }
    }

    fn load(&self) -> Vec<Workflow> {
        let p = self.path.lock().unwrap();
        std::fs::read_to_string(&*p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, wfs: &[Workflow]) {
        let p = self.path.lock().unwrap();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&*p, serde_json::to_string_pretty(wfs).unwrap_or_default());
    }

    /// Advance the workflow: returns the current step's instruction (the
    /// caller/agent executes it and calls `advance` with the output).
    pub fn advance(&self, id: &str, output: &str) -> Option<(String, String)> {
        let mut wfs = self.load();
        let wf = wfs.iter_mut().find(|w| w.id == id)?;
        if wf.done {
            return Some(("DONE".into(), String::new()));
        }
        if wf.cursor > 0 && !output.is_empty() {
            wf.steps[wf.cursor - 1].output = output.to_string();
        }
        if wf.cursor >= wf.steps.len() {
            wf.done = true;
            self.save(&wfs);
            return Some(("DONE".into(), String::new()));
        }
        let step = wf.steps[wf.cursor].clone();
        wf.cursor += 1;
        self.save(&wfs);
        Some((step.name, step.instruction))
    }
}

impl Tool for WorkflowTool {
    fn name(&self) -> &'static str {
        "workflow"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "workflow".into(),
            description: "Persistent multi-step workflows. Actions: create {name, steps:[{name,instruction}]}, list, status {id}, advance {id, output}. Workflow state persists across sessions.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create","list","status","advance","delete"]},
                    "name": {"type": "string"},
                    "steps": {"type": "array", "items": {"type": "object"}},
                    "id": {"type": "string"},
                    "output": {"type": "string", "description": "Output of the last executed step (for advance)"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let started = std::time::Instant::now();
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
        let name = args.get("name").and_then(|a| a.as_str()).unwrap_or("wf").to_string();
        let id = args.get("id").and_then(|a| a.as_str()).unwrap_or("").to_string();
        let output = args.get("output").and_then(|a| a.as_str()).unwrap_or("").to_string();

        let mut out = String::new();
        match action.as_str() {
            "create" => {
                let steps: Vec<Step> = args.get("steps")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|s| {
                        let n = s.get("name").and_then(Value::as_str).unwrap_or("step").to_string();
                        let i = s.get("instruction").and_then(Value::as_str).unwrap_or("").to_string();
                        Step { name: n, instruction: i, output: String::new() }
                    }).collect())
                    .unwrap_or_default();
                if steps.is_empty() {
                    out.push_str("ERROR: `steps` required for create\n");
                } else {
                    let mut wfs = self.load();
                    let wf_id = format!("wf-{}", wfs.len() + 1);
                    let name_clone = name.clone();
                    wfs.push(Workflow {
                        id: wf_id.clone(),
                        name,
                        steps,
                        cursor: 0,
                        state: HashMap::new(),
                        done: false,
                    });
                    self.save(&wfs);
                    out.push_str(&format!("Created {wf_id} ({name_clone})\n"));
                }
            }
            "list" => {
                let wfs = self.load();
                if wfs.is_empty() {
                    out.push_str("No workflows.\n");
                }
                for w in wfs {
                    out.push_str(&format!("{} {} step {}/{} done={}\n", w.id, w.name, w.cursor, w.steps.len(), w.done));
                }
            }
            "status" => {
                let wfs = self.load();
                match wfs.iter().find(|w| w.id == id) {
                    Some(w) => {
                        out.push_str(&format!("{} ({}): {}/{} steps done={}\n", w.id, w.name, w.cursor, w.steps.len(), w.done));
                        for (i, s) in w.steps.iter().enumerate() {
                            let marker = if i < w.cursor { "✓" } else if i == w.cursor { "→" } else { "·" };
                            out.push_str(&format!("  {marker} {} — {}\n", s.name, s.instruction.chars().take(80).collect::<String>()));
                        }
                    }
                    None => out.push_str(&format!("ERROR: no workflow {id}\n")),
                }
            }
            "advance" => {
                match self.advance(&id, &output) {
                    Some((step_name, instruction)) => {
                        if step_name == "DONE" {
                            out.push_str(&format!("Workflow {id} complete.\n"));
                        } else {
                            out.push_str(&format!("Next step: {step_name}\n{instruction}\n"));
                        }
                    }
                    None => out.push_str(&format!("ERROR: no workflow {id}\n")),
                }
            }
            "delete" => {
                let mut wfs = self.load();
                let len = wfs.len();
                wfs.retain(|w| w.id != id);
                if wfs.len() < len {
                    out.push_str(&format!("Deleted {id}\n"));
                } else {
                    out.push_str(&format!("ERROR: no workflow {id}\n"));
                }
                self.save(&wfs);
            }
            other => out.push_str(&format!("ERROR: unknown action {other:?}\n")),
        }
        let elapsed = started.elapsed().as_millis() as u64;
        Box::pin(async move { ok_outcome("", self.name(), out, elapsed) })
    }
}