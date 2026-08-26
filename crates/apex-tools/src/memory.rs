//! Memory tool: durable notes, project wiki pages, entities, FTS search.
//! Backed by apex-memory (SQLite + FTS5). The agent writes important facts
//! as notes/wiki pages; recall searches everything.

use std::sync::Mutex;

use apex_memory::{Entry, Kind, Memory};
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct MemoryTool {
    mem: Mutex<Option<Memory>>,
}

impl MemoryTool {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        let mem = Memory::open(&data_dir.join("memory")).ok();
        Self { mem: Mutex::new(mem) }
    }
}

impl Default for MemoryTool {
    fn default() -> Self {
        Self { mem: Mutex::new(None) }
    }
}

impl Tool for MemoryTool {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "memory".into(),
            description: "Durable memory: write (note/wiki/entity), search (FTS across all kinds), \
list, delete. Use `write` to persist important facts about the project or user (kind=note), \
project architecture/pages (kind=wiki), or key names (kind=entity). Use `search` before \
re-asking about facts already stored. Data lives in SQLite + FTS5.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["write", "search", "list", "get", "delete"] },
                    "kind": { "type": "string", "enum": ["note", "wiki", "entity"] },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "query": { "type": "string", "description": "search term(s)" },
                    "id": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let started = std::time::Instant::now();
        Box::pin(async move {
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("search");
            let mut guard = match self.mem.lock() {
                Ok(g) => g,
                Err(_) => return ok_outcome("", "memory", "memory store unavailable (lock poisoned)".to_string(), started.elapsed().as_millis() as u64),
            };
            let Some(mem) = guard.as_mut() else {
                return ok_outcome("", "memory", "memory store unavailable (SQLite open failed)".to_string(), started.elapsed().as_millis() as u64);
            };

            let kind = args.get("kind").and_then(|k| k.as_str()).map(Kind::from_str);
            let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
            let title = args.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let body = args.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
            let tags: Vec<String> = args.get("tags").and_then(|t| t.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();

            let out = match action {
                "write" => {
                    if body.is_empty() {
                        "ERROR: `body` required".to_string()
                    } else {
                        match mem.upsert(kind.unwrap_or(Kind::Note), &title, &body, &tags, None) {
                            Ok(id) => format!("stored #{id} [{}] {title}", kind_label(kind)),
                            Err(e) => format!("write failed: {e:#}"),
                        }
                    }
                }
                "search" => {
                    let q = args.get("query").and_then(|x| x.as_str()).unwrap_or("");
                    match mem.search(q, kind, limit) {
                        Ok(entries) => format_entries(&entries),
                        Err(e) => format!("search failed: {e:#}"),
                    }
                }
                "list" => match mem.list(kind, limit) {
                    Ok(entries) => format_entries(&entries),
                    Err(e) => format!("list failed: {e:#}"),
                },
                "get" => {
                    let id = args.get("id").and_then(|i| i.as_u64()).unwrap_or(0) as i64;
                    match mem.get(id) {
                        Ok(Some(e)) => format_entry(&e),
                        Ok(None) => format!("no entry #{id}"),
                        Err(e) => format!("get failed: {e:#}"),
                    }
                }
                "delete" => {
                    let id = args.get("id").and_then(|i| i.as_u64()).unwrap_or(0) as i64;
                    match mem.delete(id) {
                        Ok(_) => format!("deleted #{id}"),
                        Err(e) => format!("delete failed: {e:#}"),
                    }
                }
                other => format!("ERROR: unknown action {other:?}"),
            };
            ok_outcome("", "memory", out, started.elapsed().as_millis() as u64)
        })
    }
}

fn kind_label(k: Option<Kind>) -> &'static str {
    k.map(|k| k.as_str()).unwrap_or("note")
}

fn format_entry(e: &Entry) -> String {
    format!("#{} [{}] {} ({})\n{}\n", e.id, e.kind.as_str(), e.title, e.created_at, e.body)
}

fn format_entries(entries: &[Entry]) -> String {
    if entries.is_empty() {
        return "no entries found".to_string();
    }
    let mut out = String::new();
    for e in entries {
        let body_first = e.body.lines().next().unwrap_or("").chars().take(160).collect::<String>();
        out.push_str(&format!("#{} [{}] {} — {body_first}\n", e.id, e.kind.as_str(), e.title));
    }
    out
}
