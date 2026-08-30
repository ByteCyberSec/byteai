//! Todo tool: durable task list (deer-flow long-horizon pattern).
//! Persisted to JSON so tasks survive agent restarts.

use std::path::PathBuf;
use std::sync::Mutex;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct TodoTool {
    items: Mutex<Vec<TodoItem>>,
    path: PathBuf,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct TodoItem {
    id: usize,
    text: String,
    done: bool,
}

impl TodoTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let path = data_dir.join("memory").join("todo.json");
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<TodoItem>>(&s).ok())
            .unwrap_or_default();
        Self { items: Mutex::new(items), path }
    }

    fn save(&self) {
        if let Ok(items) = self.items.lock() {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.path, serde_json::to_string_pretty(&*items).unwrap_or_default());
        }
    }

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
            description: "Manage the durable task list. Actions: list, add, done, delete. Tasks persist across sessions.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "add", "done", "delete"] },
                    "text": { "type": "string", "description": "Task text (for add)." },
                    "id": { "type": "integer", "description": "Task id (for done/delete)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let started = std::time::Instant::now();
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
                    let id = items.iter().map(|t| t.id).max().unwrap_or(0) + 1;
                    items.push(TodoItem { id, text: text.clone(), done: false });
                    drop(items);
                    self.save();
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
                drop(items);
                self.save();
            }
            "delete" => {
                let id = args.get("id").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let mut items = self.items.lock().unwrap();
                let len = items.len();
                items.retain(|t| t.id != id);
                if items.len() < len {
                    out.push_str(&format!("Deleted task #{id}\n"));
                } else {
                    out.push_str(&format!("ERROR: no task #{id}\n"));
                }
                drop(items);
                self.save();
            }
            other => out.push_str(&format!("ERROR: unknown action {other:?}\n")),
        }
        let elapsed = started.elapsed().as_millis() as u64;
        Box::pin(async move { ok_outcome("", self.name(), out, elapsed) })
    }
}