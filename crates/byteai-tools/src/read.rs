//! Read tool: line-based progressive reading + AST smart modes (Phase 2).
//!
//! Modes:
//!   default          line range (offset/limit)
//!   symbols          AST outline (function/class/struct/… with line numbers)
//!   function <name>  extract just that definition's source (smart read)
//!   imports          dependency list for the file

use std::path::{Path, PathBuf};
use std::time::Instant;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

pub const MAX_LINES_PER_READ: usize = 2000;
pub const MAX_LINE_CHARS: usize = 400;

#[derive(Default)]
pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read".into(),
            description: "Read a file. Modes: default (offset/limit line range), symbols (AST outline: \
line, kind, name), function=<name> (extract one definition), imports (dependency list). \
Use symbols first to decide what to read — read less, understand more.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "description": "First line (1-based, default 1)." },
                    "limit": { "type": "integer", "description": "Max lines (default 200, max 2000)." },
                    "symbols": { "type": "boolean", "description": "AST outline instead of raw lines." },
                    "function": { "type": "string", "description": "Extract the definition with this name." },
                    "imports": { "type": "boolean", "description": "List imports/uses of the file." }
                },
                "required": ["path"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let path = match args.get("path").and_then(|p| p.as_str()) {
                Some(p) => PathBuf::from(p),
                None => return ok_outcome("", self.name(), "ERROR: missing `path`", 0),
            };
            let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
            let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).min(MAX_LINES_PER_READ as u64) as usize;
            let want_symbols = args.get("symbols").and_then(|s| s.as_bool()).unwrap_or(false);
            let want_imports = args.get("imports").and_then(|s| s.as_bool()).unwrap_or(false);
            let function = args.get("function").and_then(|f| f.as_str()).map(|s| s.to_string());

            if !path.exists() {
                return ok_outcome("", self.name(), format!("ERROR: file not found: {}", path.display()), 0);
            }

            // ── Smart modes via byteai-ast ──────────────────────────────────────
            if want_symbols {
                return match byteai_ast::symbol_summary(&path, 120) {
                    Ok(s) => ok_outcome("", self.name(), s, started.elapsed().as_millis() as u64),
                    Err(e) => ok_outcome("", self.name(), format!("ERROR: {e:#} (unsupported language?)"), started.elapsed().as_millis() as u64),
                };
            }
            if want_imports {
                return match byteai_ast::imports_file(&path) {
                    Ok(imports) => {
                        let out = if imports.is_empty() {
                            "No imports found.".to_string()
                        } else {
                            imports.join("\n")
                        };
                        ok_outcome("", self.name(), out, started.elapsed().as_millis() as u64)
                    }
                    Err(e) => ok_outcome("", self.name(), format!("ERROR: {e:#} (unsupported language?)"), started.elapsed().as_millis() as u64),
                };
            }
            if let Some(name) = function {
                let lang = match byteai_ast::language_for_path(&path) {
                    Some(l) => l,
                    None => return ok_outcome("", self.name(), format!("ERROR: unsupported language for {}", path.display()), 0),
                };
                return match std::fs::read_to_string(&path) {
                    Ok(text) => match byteai_ast::find_definition(lang, &text, &name) {
                        Some(def) => {
                            let out = format!("{name} — lines {}..{}:\n{}", def.start_line, def.end_line, def.text);
                            ok_outcome("", self.name(), out, started.elapsed().as_millis() as u64)
                        }
                        None => ok_outcome("", self.name(), format!("ERROR: no definition named {name:?} in {}", path.display()), 0),
                    },
                    Err(e) => ok_outcome("", self.name(), format!("ERROR reading {}: {e}", path.display()), 0),
                };
            }

            // ── Default line-range read ───────────────────────────────────────
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => return ok_outcome("", self.name(), format!("ERROR reading {}: {e}", path.display()), 0),
            };
            let total = text.lines().count();
            let start = offset.saturating_sub(1);
            let mut out = String::new();
            let mut shown = 0usize;
            for (i, line) in text.lines().skip(start).take(limit).enumerate() {
                shown += 1;
                let lno = start + i + 1;
                let lc: String = line.chars().take(MAX_LINE_CHARS).collect();
                out.push_str(&format!("{lno:>6}| {lc}\n"));
            }
            let truncated = shown > 0 && start + shown < total;
            out.push_str(&format!(
                "[{}: {} lines shown (offset {offset}, limit {limit}); file has {total} lines{}]",
                path.display(),
                shown,
                if truncated { format!("; use read with offset {} to continue", start + shown + 1) } else { String::new() }
            ));
            ok_outcome("", self.name(), out, started.elapsed().as_millis() as u64)
        })
    }
}

#[allow(dead_code)]
fn _path_helper(_: &Path) {}
