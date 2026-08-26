//! Todo tool: simple task list for the agent's working state (Layer A seed).

use std::sync::Mutex;
use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Default)]
pub struct TodoTool {
    items: Mutex<Vec<TodoItem>>,
}

#[derive(Clone)]
struct TodoItem {
    id: usize,
    text: String,
    done: bool,
}

impl TodoTool {
    fn snapshot(&self) -> Vec<TodoItem> {
        self.items.lock().unwrap().clone()
    }
}

impl Tool for TodoTool {
    fn name(&self) -> &'static str {
        "todo"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "todo".into(),
            description: "Manage the task list. Actions: list, add, done. "
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "add", "done"] },
                    "text": { "type": "string", "description": "Task text (for add)." },
                    "id": { "type": "integer", "description": "Task id (for done)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
            let mut out = String::new();
            match action.as_str() {
                "list" => {
                    let items = self.snapshot();
                    if items.is_empty() {
                        out.push_str("No tasks.\n");
                    }
                    for it in items {
                        out.push_str(&format!("[{}] #{} {}\n", if it.done { "x" } else { " " }, it.id, it.text));
                    }
                }
                "add" => {
                    let text = args.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    if text.is_empty() {
                        out.push_str("ERROR: `text` required for add\n");
                    } else {
                        let mut items = self.items.lock().unwrap();
                        let id = items.len() + 1;
                        items.push(TodoItem { id, text: text.clone(), done: false });
                        out.push_str(&format!("Added task #{id}: {text}\n"));
                    }
                }
                "done" => {
                    let id = args.get("id").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    let mut items = self.items.lock().unwrap();
                    match items.iter_mut().find(|t| t.id == id) {
                        Some(t) => {
                            t.done = true;
                            out.push_str(&format!("Done task #{id}: {}\n", t.text));
                        }
                        None => out.push_str(&format!("ERROR: no task #{id}\n")),
                    }
                }
                other => out.push_str(&format!("ERROR: unknown action {other:?}\n")),
            }
            let elapsed = started.elapsed().as_millis() as u64;
            ok_outcome("", self.name(), out, elapsed)
        })
    }
}
