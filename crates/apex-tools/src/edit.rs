//! Edit tool: multi-strategy replacement with LSP validation (Phase 2).
//!
//! Strategy ladder (ADR-0003): exact → contextual (whitespace-tolerant) →
//! whole-file. After applying, when an LSP server is available for the
//! file's language, diagnostics are fetched and surfaced so the agent can
//! repair its own edit — the "EDIT → diagnostics → repair" loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apex_lsp::LspRegistry;
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct EditTool {
    lsp: Option<Arc<LspRegistry>>,
}

impl EditTool {
    pub fn new(lsp: Option<Arc<LspRegistry>>) -> Self {
        Self { lsp }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "edit".into(),
            description: "Replace text in a file. `old` must match exactly once (or set allow_multiple=true). \
If not found exactly, retries with whitespace-insensitive matching (strategy=contextual), then whole-file \
(strategy=whole with `new` as the entire new content). Set validate=true to run LSP diagnostics after the edit. \
Never silently rewrite a file: whole-file requires explicit strategy=whole.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old": { "type": "string", "description": "Text to replace (exact)." },
                    "new": { "type": "string", "description": "Replacement text (or entire file when strategy=whole)." },
                    "allow_multiple": { "type": "boolean", "default": false },
                    "strategy": { "type": "string", "enum": ["auto", "exact", "contextual", "whole"], "default": "auto" },
                    "validate": { "type": "boolean", "default": true, "description": "Run LSP diagnostics after edit when a server is available." }
                },
                "required": ["path", "old", "new"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let started = Instant::now();
            let path = match args.get("path").and_then(|p| p.as_str()) {
                Some(p) => p.to_string(),
                None => return ok_outcome("", "edit", "ERROR: missing `path`", 0),
            };
            let old = args.get("old").and_then(|o| o.as_str()).unwrap_or("").to_string();
            let new = args.get("new").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let allow_multiple = args.get("allow_multiple").and_then(|a| a.as_bool()).unwrap_or(false);
            let strategy = args.get("strategy").and_then(|s| s.as_str()).unwrap_or("auto");
            let validate = args.get("validate").and_then(|v| v.as_bool()).unwrap_or(true);

            if old.is_empty() && strategy != "whole" {
                return ok_outcome("", "edit", "ERROR: `old` must not be empty (unless strategy=whole)", 0);
            }

            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => return ok_outcome("", "edit", format!("ERROR reading {path}: {e}"), 0),
            };

            // ── Strategy ladder ────────────────────────────────────────────────
            let applied: Option<(String, String, usize, String)> = match strategy {
                "whole" => {
                    // Whole-file: `new` IS the file. Only reachable via explicit strategy.
                    Some((text.clone(), new.clone(), 1, "whole-file".into()))
                }
                _ => {
                    // 1) exact
                    let count = text.matches(&old).count();
                    if count > 0 {
                        if count > 1 && !allow_multiple {
                            return ok_outcome(
                                "",
                                "edit",
                                format!("ERROR: `old` matches {count} times in {path}. Include more context, set allow_multiple=true, or use strategy=contextual/whole."),
                                0,
                            );
                        }
                        let new_text = if allow_multiple { text.replace(&old, &new) } else { text.replacen(&old, &new, 1) };
                        Some((text.clone(), new_text, if allow_multiple { count } else { 1 }, "exact".into()))
                    } else if strategy == "exact" {
                        return ok_outcome(
                            "",
                            "edit",
                            format!("ERROR: `old` text not found exactly in {path}. Try search first, or strategy=contextual/whole."),
                            0,
                        );
                    } else {
                        // 2) contextual: whitespace-tolerant match
                        match contextual_replace(&text, &old, &new, allow_multiple) {
                            Ok((new_text, n)) => Some((text.clone(), new_text, n, "contextual".into())),
                            Err(msg) => {
                                return ok_outcome("", "edit", format!("ERROR: {msg} (tried exact + contextual). Use strategy=whole for full-file rewrite."), 0);
                            }
                        }
                    }
                }
            };

            let (old_text, new_text, occ, strategy_name) = applied.unwrap();
            // Preserve trailing newline if present.
            let new_text = if old_text.ends_with('\n') && !new_text.ends_with('\n') {
                new_text + "\n"
            } else {
                new_text
            };

            if let Err(e) = std::fs::write(&path, &new_text) {
                return ok_outcome("", "edit", format!("ERROR writing {path}: {e}"), 0);
            }

            let mut out = String::new();
            out.push_str(&format!("Applied {strategy_name} edit to {path} ({} occurrence(s)).\n", occ));
            out.push_str(&format!("Before:\n{}\nAfter:\n{}", preview(&old, 200), preview(&new, 200)));

            // ── LSP validation (optional, best-effort) ────────────────────────
            if validate {
                if let Some(registry) = &lsp {
                    let p = PathBuf::from(&path);
                    let lang = apex_lsp::language_for_path(&p);
                    if let Some(lang) = lang {
                        if registry.supports(&lang) {
                            match lsp_diag(registry, &p, &new_text, &lang).await {
                                Ok(diags) => {
                                    let n_err = diags.iter().filter(|d| d.severity == Some(1)).count();
                                    let n_warn = diags.iter().filter(|d| d.severity == Some(2)).count();
                                    out.push_str(&format!("\nLSP diagnostics: {n_err} errors, {n_warn} warnings."));
                                    for d in diags.iter().take(8) {
                                        let sev = match d.severity { Some(1) => "E", Some(2) => "W", _ => "I" };
                                        out.push_str(&format!("\n  {sev} {}:{}  {}", d.range.0 + 1, d.range.1 + 1, d.message));
                                    }
                                    if diags.len() > 8 {
                                        out.push_str(&format!("\n  … {} more", diags.len() - 8));
                                    }
                                    if n_err > 0 {
                                        out.push_str("\nWARNING: edit introduces errors — consider a repair pass.");
                                    }
                                }
                                Err(e) => out.push_str(&format!("\nLSP validation skipped: {e:#}")),
                            }
                        }
                    }
                }
            }

            ok_outcome("", "edit", out, started.elapsed().as_millis() as u64)
        })
    }
}

/// Whitespace-tolerant replacement: normalize both sides (collapse runs of
/// whitespace to single spaces, trim), find the span in the original by
/// matching line-blocks; replace the ORIGINAL span so formatting survives.
fn contextual_replace(text: &str, old: &str, new: &str, allow_multiple: bool) -> Result<(String, usize), String> {
    let norm = |s: &str| -> String {
        s.lines()
            .map(|l| l.trim().split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let n_old = norm(old);
    let n_text = norm(text);
    if n_old.is_empty() {
        return Err("contextual: `old` is empty after normalization".into());
    }
    let mut count = 0usize;
    let mut idx = 0usize;
    while let Some(rel) = n_text[idx..].find(&n_old) {
        count += 1;
        idx += rel + n_old.len();
    }
    if count == 0 {
        return Err("contextual: no whitespace-insensitive match found".into());
    }
    if count > 1 && !allow_multiple {
        return Err(format!("contextual: matches {count} times; include more context or allow_multiple=true"));
    }

    // Apply to ORIGINAL text: locate the byte span of the first (or every)
    // normalized occurrence by walking original lines.
    let mut result = text.to_string();
    if allow_multiple {
        // Replace every match: walk normalized mapping repeatedly.
        let mut done = 0usize;
        while done < count {
            let (span, _) = locate_span(&result, old, &norm)?;
            result.replace_range(span, new);
            done += 1;
        }
    } else {
        let (span, _) = locate_span(&result, old, &norm)?;
        result.replace_range(span, new);
    }
    Ok((result, count))
}

/// Locate the byte range in `text` whose normalized form equals normalized `old`.
fn locate_span(text: &str, old: &str, norm: &impl Fn(&str) -> String) -> Result<(std::ops::Range<usize>, String), String> {
    let n_old = norm(old);
    let lines: Vec<&str> = text.lines().collect();
    let n_lines: Vec<String> = lines.iter().map(|l| norm(l)).collect();
    // Join with \n and search for the normalized block; expand outward to
    // include leading whitespace of the first line (so indentation survives).
    let joined = n_lines.join("\n");
    let pos = joined.find(&n_old).ok_or_else(|| "normalized block not found".to_string())?;
    // Map joined position -> (line_idx, col in normalized line).
    let mut line_idx = 0usize;
    let mut col = 0usize;
    for (i, nl) in n_lines.iter().enumerate() {
        let seg_len = nl.len() + 1; // + newline
        if pos < seg_len || (i == n_lines.len() - 1) {
            line_idx = i;
            col = pos.saturating_sub(if i > 0 { n_lines[..i].iter().map(|s| s.len() + 1).sum::<usize>() } else { 0 });
            break;
        }
    }
    let _ = col;
    // Start byte: beginning of that original line (including indentation).
    let mut byte_start = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if i == line_idx {
            break;
        }
        byte_start += l.len() + 1;
    }
    // End byte: end of the last normalized-matched original line.
    let n_end = pos + n_old.len();
    let mut end_line = line_idx;
    let mut acc = if line_idx > 0 { n_lines[..line_idx].iter().map(|s| s.len() + 1).sum::<usize>() } else { 0 };
    for (i, nl) in n_lines.iter().enumerate().skip(line_idx) {
        let seg = acc + nl.len() + 1;
        if n_end <= seg {
            end_line = i;
            break;
        }
        acc = seg;
    }
    let mut byte_end = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if i > end_line {
            break;
        }
        byte_end += l.len() + 1;
    }
    let span = byte_start..byte_end;
    Ok((span, n_old))
}

/// Fetch LSP diagnostics for a file (open + change + wait).
async fn lsp_diag(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str) -> anyhow::Result<Vec<apex_lsp::Diagnostic>> {
    let root = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let state = registry.get(lang, &root).await?;
    let mut st = state.lock().await;
    match &mut *st {
        apex_lsp::ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            let _ = s.did_change(path, text, 1).await;
            Ok(s.wait_diagnostics(path, Duration::from_secs(10)).await)
        }
        apex_lsp::ServerState::Unavailable(reason) => Err(anyhow::anyhow!("unavailable: {reason}")),
        apex_lsp::ServerState::Spawning => Err(anyhow::anyhow!("still spawning")),
    }
}

fn preview(s: &str, max: usize) -> String {
    let c: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{c}…")
    } else {
        c
    }
}