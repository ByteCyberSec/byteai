//! `autocontext` — context governor (unique to ByteAI).
//!
//! Manages the spill artifacts that the core loop creates when tool output
//! exceeds the character budget. The agent can discover what was spilled,
//! recall full content, archive important spills to memory, and prune old
//! ones. This is a "context governor" — the agent manages its own context
//! window, something no other agent harness provides as a tool.
//!
//! Spill files live under `<data>/spill/<call_id>.txt` (created by the core
//! loop's `spill_tool_output` function).

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct AutoContextTool {
    spill_dir: PathBuf,
    data_dir: PathBuf,
}

impl AutoContextTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let spill_dir = data_dir.join("spill");
        let _ = std::fs::create_dir_all(&spill_dir);
        Self { spill_dir, data_dir }
    }

    fn spill_files(&self) -> Vec<std::path::PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(&self.spill_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("txt"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by_key(|p| p.metadata().and_then(|m| m.modified()).ok().unwrap_or(std::time::SystemTime::UNIX_EPOCH));
        files
    }
}

impl Tool for AutoContextTool {
    fn name(&self) -> &'static str {
        "autocontext"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "autocontext".into(),
            description: "Context governor: manage what's in the agent's context window. \
                Actions: status (show spill files + estimated size), \
                recall <id> (read a spilled artifact's full content), \
                archive <id> <name> (save a spill to memory as a note, then delete it), \
                prune <hours> (delete spill files older than N hours). \
                Use when you need to retrieve data that was spilled to save context budget."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["status", "recall", "archive", "prune"], "description": "What to do." },
                    "id": { "type": "string", "description": "Spill file name (without .txt) or prefix for recall/archive." },
                    "name": { "type": "string", "description": "Memory note name for archive." },
                    "hours": { "type": "integer", "description": "Max age in hours for prune (default 24)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let spill_dir = self.spill_dir.clone();
        let data_dir = self.data_dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("status").to_string();
            let id = args.get("id").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let name = args.get("name").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let hours = args.get("hours").and_then(|a| a.as_u64()).unwrap_or(24);
            let elapsed = started.elapsed().as_millis() as u64;

            let t = AutoContextTool { spill_dir: spill_dir.clone(), data_dir: data_dir.clone() };

            match action.as_str() {
                "status" => {
                    let files = t.spill_files();
                    if files.is_empty() {
                        return ok_outcome(
                            "",
                            "autocontext",
                            "no spill files — tool output has not exceeded the character budget recently.\n\
                             Spill files are created automatically when a tool produces more than \n\
                             max_tool_output_chars bytes. You can set max_tool_output_chars in config.toml\n\
                             to control the threshold."
                                .to_string(),
                            elapsed,
                        );
                    }
                    let total_bytes: u64 = files.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum();
                    let mut out = format!("{} spill file(s) — {} total bytes\n", files.len(), total_bytes);
                    for f in &files {
                        let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        let age = f
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .map(|m| {
                                m.elapsed()
                                    .map(|d| format!("{:.1}h", d.as_secs_f64() / 3600.0))
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default();
                        out.push_str(&format!("  {name} — {size}B — {age} old\n", name = name, size = size, age = age));
                    }
                    out.push_str("Use `autocontext recall <id>` to read, `autocontext archive <id> <name>` to save as memory, `autocontext prune <hours>` to clean up.");
                    ok_outcome("", "autocontext", out, elapsed)
                }
                "recall" => {
                    if id.is_empty() {
                        return ok_outcome("", "autocontext", "usage: autocontext recall <id>".to_string(), elapsed);
                    }
                    let path = t.spill_dir.join(format!("{id}.txt"));
                    if !path.exists() {
                        return ok_outcome("", "autocontext", format!("no spill file {id}.txt — check autocontext status for available ids"), elapsed);
                    }
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let size = content.len();
                    let preview = if size > 12000 {
                        format!("{} (showing first 12k of {}):\n{}", id, size, &content[..12000])
                    } else {
                        format!("{} ({}B):\n{}", id, size, content)
                    };
                    ok_outcome("", "autocontext", preview, elapsed)
                }
                "archive" => {
                    if id.is_empty() || name.is_empty() {
                        return ok_outcome("", "autocontext", "usage: autocontext archive <id> <name>".to_string(), elapsed);
                    }
                    let path = t.spill_dir.join(format!("{id}.txt"));
                    if !path.exists() {
                        return ok_outcome("", "autocontext", format!("no spill file {id}.txt"), elapsed);
                    }
                    let content = std::fs::read_to_string(&path).unwrap_or_default();
                    let notes_dir = data_dir.join("memory").join("notes");
                    let _ = std::fs::create_dir_all(&notes_dir);
                    let note_path = notes_dir.join(format!("{}.md", name));
                    match std::fs::write(&note_path, format!("# {}\n\nAuto-archived from spill {id}\n\n{content}", name)) {
                        Ok(_) => {
                            let _ = std::fs::remove_file(&path);
                            ok_outcome("", "autocontext", format!("archived {id} → memory note {name} ({note_path:?})", note_path = note_path), elapsed)
                        }
                        Err(e) => ok_outcome("", "autocontext", format!("could not archive: {e}"), elapsed),
                    }
                }
                "prune" => {
                    let cutoff = std::time::SystemTime::now()
                        .checked_sub(std::time::Duration::from_secs(hours * 3600))
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let files = t.spill_files();
                    let mut removed = 0usize;
                    let mut bytes = 0u64;
                    for f in &files {
                        if let Ok(m) = f.metadata() {
                            if let Ok(modified) = m.modified() {
                                if modified < cutoff {
                                    bytes += m.len();
                                    let _ = std::fs::remove_file(f);
                                    removed += 1;
                                }
                            }
                        }
                    }
                    ok_outcome(
                        "",
                        "autocontext",
                        format!("pruned {removed} spill file(s) ({bytes}B freed) — {hours}h cutoff"),
                        elapsed,
                    )
                }
                other => ok_outcome("", "autocontext", format!("unknown action {other:?} — use status | recall | archive | prune"), elapsed),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_acx_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn status_recall_archive_prune_roundtrip() {
        let d = tmp_dir("roundtrip");
        let spill = d.join("spill");
        std::fs::create_dir_all(&spill).unwrap();
        std::fs::write(spill.join("test1.txt"), "hello world").unwrap();
        std::fs::write(spill.join("test2.txt"), "some tool output\nline 2\nline 3").unwrap();

        let t = AutoContextTool::new(d.clone());

        // status shows both files
        let out = t.execute(json!({"action": "status"})).await;
        assert!(out.output.contains("2 spill file(s)"), "status shows 2: {}", out.output);
        assert!(out.output.contains("test1"), "test1 listed: {}", out.output);
        assert!(out.output.contains("test2"), "test2 listed: {}", out.output);

        // recall reads content
        let out = t.execute(json!({"action": "recall", "id": "test1"})).await;
        assert!(out.output.contains("hello world"), "recall shows content: {}", out.output);

        // archive moves to memory notes
        let out = t.execute(json!({"action": "archive", "id": "test2", "name": "saved-output"})).await;
        assert!(out.output.contains("archived"), "archive: {}", out.output);
        assert!(!spill.join("test2.txt").exists(), "spill file removed after archive");
        assert!(d.join("memory").join("notes").join("saved-output.md").exists(), "note created");

        // prune removes the remaining file (the archive already removed test2)
        // Make test1 old enough by touching it far in the past
        let _ = std::fs::write(spill.join("test1.txt"), "still here"); // recreate if needed
        let out = t.execute(json!({"action": "prune", "hours": 0})).await;
        assert!(out.output.contains("1 spill file"), "prune: {}", out.output);
        assert!(!spill.join("test1.txt").exists(), "test1 pruned");

        let _ = std::fs::remove_dir_all(&d);
    }
}