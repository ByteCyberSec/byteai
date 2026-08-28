//! Search tool: tier-1 literal and tier-2 regex search over a directory tree.

use std::path::{Path, PathBuf};
use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use regex::Regex;
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".venv", "venv", "__pycache__", ".next", ".turbo", ".pytest_cache"];
const DEFAULT_MAX_MATCHES: usize = 50;
const MATCH_LINE_CHARS: usize = 220;
const MAX_FILE_BYTES_SCAN: u64 = 4 * 1024 * 1024; // skip files larger than 4 MiB

#[derive(Default)]
pub struct SearchTool;

impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "search"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "search".into(),
            description: "Search files under a directory. mode 'literal' (default), 'regex', or 'symbol' \
(symbol/definition names via AST). Returns path:line matches (capped). "
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Literal text, regex pattern, or symbol name (mode=symbol)." },
                    "path": { "type": "string", "description": "Directory to search (default: current directory)." },
                    "mode": { "type": "string", "enum": ["literal", "regex", "symbol"], "default": "literal" },
                    "max_matches": { "type": "integer", "default": 50 },
                    "glob": { "type": "string", "description": "Optional file extension filter like '*.rs' or '*.py'." }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
                Some(p) => p.to_string(),
                None => return ok_outcome("", self.name(), "ERROR: missing `pattern`", 0),
            };
            let root = args
                .get("path")
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let mode = args.get("mode").and_then(|m| m.as_str()).unwrap_or("literal");
            let max_matches = args
                .get("max_matches")
                .and_then(|m| m.as_u64())
                .unwrap_or(DEFAULT_MAX_MATCHES as u64)
                .min(500) as usize;
            let glob = args.get("glob").and_then(|g| g.as_str()).map(String::from);

            // ── Tier 3: symbol search via AST (no LSP needed) ─────────────────
            if mode == "symbol" {
                let needle = pattern.to_lowercase();
                let mut hits = Vec::new();
                let mut files_scanned = 0usize;
                walk(&root, &mut |path| {
                    if let Some(g) = &glob {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if !glob_match(g, name) {
                            return false;
                        }
                    }
                    if apex_ast::language_for_path(path).is_none() {
                        return false;
                    }
                    files_scanned += 1;
                    if let Ok(syms) = apex_ast::extract_file(path) {
                        for s in syms.iter().take(200) {
                            if s.kind == apex_ast::SymbolKind::Import {
                                continue;
                            }
                            if s.name.to_lowercase().contains(&needle) {
                                hits.push(format!("{}:{}:{} {}", path.display(), s.start_line, s.kind.as_str(), s.name));
                                if hits.len() >= max_matches {
                                    return true;
                                }
                            }
                        }
                    }
                    false
                });
                let elapsed = started.elapsed().as_millis() as u64;
                let mut out = String::new();
                if hits.is_empty() {
                    out.push_str(&format!("No symbol matches for {pattern:?} under {} ({} files parsed)\n", root.display(), files_scanned));
                } else {
                    out.push_str(&format!("{} symbol match(es) for {pattern:?} under {} ({} files parsed):\n", hits.len(), root.display(), files_scanned));
                    for h in &hits {
                        out.push_str(h);
                        out.push('\n');
                    }
                    if hits.len() == max_matches {
                        out.push_str(&format!("[capped at {max_matches}; refine the pattern or add glob]"));
                    }
                }
                return ok_outcome("", self.name(), out, elapsed);
            }

            let re = if mode == "regex" {
                match Regex::new(&pattern) {
                    Ok(r) => Some(r),
                    Err(e) => return ok_outcome("", self.name(), format!("ERROR: invalid regex: {e}"), 0),
                }
            } else {
                None
            };

            let mut matches = Vec::new();
            let mut files_scanned = 0usize;
            walk(&root, &mut |path| {
                if let Some(g) = &glob {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !glob_match(g, name) {
                        return false;
                    }
                }
                files_scanned += 1;
                if let Ok(text) = read_text(path) {
                    for (i, line) in text.lines().enumerate() {
                        let hit = match &re {
                            Some(r) => r.is_match(line),
                            None => line.contains(&pattern),
                        };
                        if hit {
                            let lc: String = line.chars().take(MATCH_LINE_CHARS).collect();
                            matches.push(format!("{}:{}: {}", path.display(), i + 1, lc));
                            if matches.len() >= max_matches {
                                return true; // stop
                            }
                        }
                    }
                }
                false
            });

            let elapsed = started.elapsed().as_millis() as u64;
            let mut out = String::new();
            if matches.is_empty() {
                out.push_str(&format!("No matches for {mode:?} pattern {pattern:?} under {} ({files_scanned} files scanned)\n", root.display()));
            } else {
                out.push_str(&format!("{} match(es) for {mode:?} pattern {pattern:?} under {} ({} files scanned):\n", matches.len(), root.display(), files_scanned));
                for m in &matches {
                    out.push_str(m);
                    out.push('\n');
                }
                if matches.len() == max_matches {
                    out.push_str(&format!("[capped at {max_matches} matches; refine the pattern or add glob]"));
                }
            }
            ok_outcome("", self.name(), out, elapsed)
        })
    }
}

/// Recursive walk that stops early when the visitor returns true.
fn walk(dir: &Path, visitor: &mut impl FnMut(&Path) -> bool) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            if visitor(&path) {
                return;
            }
            walk(&path, visitor);
        } else if ft.is_file()
            && visitor(&path) {
                return;
            }
    }
}

fn read_text(path: &Path) -> std::io::Result<String> {
    if path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES_SCAN {
        return Ok(String::new());
    }
    let bytes = std::fs::read(path)?;
    if bytes.contains(&0) {
        return Ok(String::new()); // binary
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Minimal glob: '*' matches within a name; otherwise exact.
fn glob_match(glob: &str, name: &str) -> bool {
    if glob == "*" {
        return true;
    }
    if let Some(ext) = glob.strip_prefix("*.") {
        return name.ends_with(ext);
    }
    name == glob
}
