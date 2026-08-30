//! Note tool: minimal Layer-B memory seed — durable markdown notes under
//! `<data>/memory/notes/`. Full memory system lands in Phase 5.

use std::path::{Path, PathBuf};
use std::time::Instant;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct NoteTool {
    dir: PathBuf,
}

impl NoteTool {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

impl Tool for NoteTool {
    fn name(&self) -> &'static str {
        "note"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "note".into(),
            description: "Persist a durable project note (memory Layer B). Actions: write, read, list. Notes live in .byteai/memory/notes/. "
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["write", "read", "list"] },
                    "name": { "type": "string", "description": "Note name (e.g. 'architecture', 'conventions')." },
                    "content": { "type": "string", "description": "Markdown content (for write)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let dir = self.dir.clone();
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
            let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let mut out = String::new();

            match action.as_str() {
                "write" => {
                    if name.is_empty() {
                        out.push_str("ERROR: `name` required for write\n");
                    } else {
                        let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                        let content_len = content.len();
                        let safe = sanitize_name(&name);
                        let path = dir.join(format!("{safe}.md"));
                        if let Err(e) = std::fs::create_dir_all(dir.as_path()) {
                            out.push_str(&format!("ERROR: {e}\n"));
                        } else if let Err(e) = std::fs::write(&path, content) {
                            out.push_str(&format!("ERROR writing {path:?}: {e}\n"));
                        } else {
                            out.push_str(&format!("Wrote note {} ({safe}.md, {content_len} bytes)\n", name));
                        }
                    }
                }
                "read" => {
                    if name.is_empty() {
                        out.push_str("ERROR: `name` required for read\n");
                    } else {
                        let safe = sanitize_name(&name);
                        let path = dir.join(format!("{safe}.md"));
                        match std::fs::read_to_string(&path) {
                            Ok(t) => out.push_str(&t),
                            Err(e) => out.push_str(&format!("ERROR reading {path:?}: {e}\n")),
                        }
                    }
                }
                "list" => {
                    let _ = std::fs::create_dir_all(dir.as_path());
                    match std::fs::read_dir(&dir) {
                        Ok(entries) => {
                            let mut names: Vec<String> = entries
                                .flatten()
                                .filter_map(|e| {
                                    let n = e.file_name().to_string_lossy().into_owned();
                                    n.strip_suffix(".md").map(String::from)
                                })
                                .collect();
                            names.sort();
                            if names.is_empty() {
                                out.push_str("No notes yet.\n");
                            }
                            for n in names {
                                out.push_str(&format!("- {n}\n"));
                            }
                        }
                        Err(e) => out.push_str(&format!("ERROR: {e}\n")),
                    }
                }
                other => out.push_str(&format!("ERROR: unknown action {other:?}\n")),
            }
            let elapsed = started.elapsed().as_millis() as u64;
            ok_outcome("", self.name(), out, elapsed)
        })
    }
}

fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if cleaned.is_empty() {
        "note".into()
    } else {
        cleaned
    }
}

#[allow(dead_code)]
fn _p(_: &Path) {}
