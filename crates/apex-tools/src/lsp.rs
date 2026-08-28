//! LSP tool: diagnostics, symbols, hover, definition, references, rename,
//! formatting. Degrades to "unavailable" when no server exists.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use apex_lsp::{LspRegistry, LspServer, ServerState, language_for_path};
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct LspTool {
    lsp: Option<Arc<LspRegistry>>,
}

impl LspTool {
    pub fn new(lsp: Option<Arc<LspRegistry>>) -> Self {
        Self { lsp }
    }
}

fn def() -> ToolDef {
    ToolDef {
        name: "lsp".to_string(),
        description: "Language-server intelligence. Actions: status, diagnostics <path>, \
symbols <path>, hover <path> <line> <col>, definition <path> <line> <col>, \
references <path> <line> <col>, rename <path> <line> <col> <name>, format <path>.\
Lines/cols 1-based. Returns ERROR when unavailable.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["status", "diagnostics", "symbols", "hover", "definition", "references", "rename", "format"] },
                "path": { "type": "string" },
                "line": { "type": "integer" },
                "col": { "type": "integer" },
                "name": { "type": "string" }
            },
            "required": ["action"]
        }),
    }
}

impl Tool for LspTool {
    fn name(&self) -> &'static str { "lsp" }
    fn def(&self) -> ToolDef { def() }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("status");
            let path = args.get("path").and_then(|p| p.as_str()).map(PathBuf::from);

            let Some(registry) = lsp else {
                return ok_outcome("", "lsp", "LSP disabled (no registry configured).", started.elapsed().as_millis() as u64);
            };

            if action == "status" {
                let langs = registry.available();
                return ok_outcome("", "lsp", format!("LSP servers available: {}", if langs.is_empty() { "none".into() } else { langs.join(", ") }), started.elapsed().as_millis() as u64);
            }

            let Some(path) = path else {
                return ok_outcome("", "lsp", "ERROR: `path` required".to_string(), started.elapsed().as_millis() as u64);
            };
            if !path.exists() {
                return ok_outcome("", "lsp", format!("ERROR: file not found: {}", path.display()), started.elapsed().as_millis() as u64);
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => return ok_outcome("", "lsp", format!("ERROR reading: {e}"), started.elapsed().as_millis() as u64),
            };

            let lang = match language_for_path(&path) {
                Some(l) => l.to_string(),
                None => return ok_outcome("", "lsp", format!("ERROR: no LSP language for {}", path.extension().and_then(|e| e.to_str()).unwrap_or("?")), started.elapsed().as_millis() as u64),
            };
            let root = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));

            let result = match action {
                "diagnostics" => diag_action(&registry, &path, &text, &lang, &root).await,
                "symbols" => sym_action(&registry, &path, &text, &lang, &root).await,
                "hover" => {
                    let (l, c) = pos_args(&args);
                    let p = path.clone();
                    lsp_action(&registry, &path, &text, &lang, &root, |s| Box::pin(async move { s.hover(&p, l.saturating_sub(1), c.saturating_sub(1)).await })).await
                }
                "definition" => {
                    let (l, c) = pos_args(&args);
                    def_action(&registry, &path, &text, &lang, &root, l, c).await
                }
                "references" => {
                    let (l, c) = pos_args(&args);
                    refs_action(&registry, &path, &text, &lang, &root, l, c).await
                }
                "rename" => {
                    let (l, c) = pos_args(&args);
                    let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    rename_action(&registry, &path, &text, &lang, &root, l, c, &name).await
                }
                "format" => fmt_action(&registry, &path, &text, &lang, &root).await,
                other => format!("ERROR: unknown action {other:?}"),
            };
            ok_outcome("", "lsp", result, started.elapsed().as_millis() as u64)
        })
    }
}

// ── Per-action helpers (avoid async closure lifetime issues) ────────────────

#[allow(dead_code)]
async fn with_server<T>(
    registry: &LspRegistry,
    lang: &str,
    root: &Path,
    path: &std::path::Path,
    text: &str,
    f: impl for<'a> FnOnce(&'a mut LspServer) -> apex_lsp::LspFuture<'a, T>,
) -> Result<T, String> {
    let state = registry.get(lang, root).await.map_err(|e| format!("{e:#}"))?;
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            let _ = s.did_change(path, text, 1).await;
            f(s).await.map_err(|e| format!("{e:#}"))
        }
        ServerState::Unavailable(reason) => Err(format!("unavailable: {reason}")),
        ServerState::Spawning => Err("still spawning".to_string()),
    }
}

async fn diag_action(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str, root: &Path) -> String {
    let state = match registry.get(lang, root).await {
        Ok(s) => s,
        Err(e) => return format!("LSP unavailable: {e:#}"),
    };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            let _ = s.did_change(path, text, 1).await;
            let diags = s.wait_diagnostics(path, Duration::from_secs(10)).await;
            drop(st);
            format_diags(&diags)
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

async fn sym_action(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str, root: &Path) -> String {
    let state = match registry.get(lang, root).await {
        Ok(s) => s,
        Err(e) => return format!("LSP unavailable: {e:#}"),
    };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            match s.document_symbols(path).await {
                Ok(syms) => {
                    if syms.is_empty() { return "No symbols".to_string(); }
                    let mut out = String::new();
                    for sym in syms.iter().take(60) {
                        let detail = sym.detail.clone().map(|d| format!("  // {d}")).unwrap_or_default();
                        out.push_str(&format!("{:>6}:{:<4} {:<20} {}{}\n", sym.start_line, sym.start_col, sym.name, kind_name(sym.kind), detail));
                    }
                    if syms.len() > 60 { out.push_str(&format!("… {} more\n", syms.len() - 60)); }
                    out
                }
                Err(e) => format!("LSP error: {e:#}"),
            }
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

async fn lsp_action<T: std::fmt::Display>(
    registry: &LspRegistry,
    path: &std::path::Path,
    text: &str,
    lang: &str,
    root: &Path,
    f: impl for<'a> FnOnce(&'a mut LspServer) -> apex_lsp::LspFuture<'a, T>,
) -> String {
    let state = match registry.get(lang, root).await {
        Ok(s) => s,
        Err(e) => return format!("LSP unavailable: {e:#}"),
    };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            let _ = s.did_change(path, text, 1).await;
            match f(s).await {
                Ok(v) => format!("{v}"),
                Err(e) => format!("LSP error: {e:#}"),
            }
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

async fn def_action(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str, root: &Path, l: usize, c: usize) -> String {
    let state = match registry.get(lang, root).await { Ok(s) => s, Err(e) => return format!("LSP unavailable: {e:#}") };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            match s.definition(path, l.saturating_sub(1), c.saturating_sub(1)).await {
                Ok(locs) if locs.is_empty() => "No definition found".to_string(),
                Ok(locs) => locs.iter().take(10).map(|x| format!("{}:{}:{}", x.uri, x.start_line, x.start_col)).collect::<Vec<_>>().join("\n"),
                Err(e) => format!("LSP error: {e:#}"),
            }
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

async fn refs_action(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str, root: &Path, l: usize, c: usize) -> String {
    let state = match registry.get(lang, root).await { Ok(s) => s, Err(e) => return format!("LSP unavailable: {e:#}") };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            match s.references(path, l.saturating_sub(1), c.saturating_sub(1), false).await {
                Ok(locs) if locs.is_empty() => "No references".to_string(),
                Ok(locs) => locs.iter().take(30).map(|x| format!("{}:{}:{}", x.uri, x.start_line, x.start_col)).collect::<Vec<_>>().join("\n"),
                Err(e) => format!("LSP error: {e:#}"),
            }
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn rename_action(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str, root: &Path, l: usize, c: usize, name: &str) -> String {
    let state = match registry.get(lang, root).await { Ok(s) => s, Err(e) => return format!("LSP unavailable: {e:#}") };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            match s.rename(path, l.saturating_sub(1), c.saturating_sub(1), name).await {
                Ok(edit) => {
                    let changes = edit.get("changes").and_then(|c| c.as_object()).map(|o| o.len()).unwrap_or(0);
                    format!("Rename: {changes} file(s) affected; apply via write (advanced)")
                }
                Err(e) => format!("LSP error: {e:#}"),
            }
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

async fn fmt_action(registry: &LspRegistry, path: &std::path::Path, text: &str, lang: &str, root: &Path) -> String {
    let state = match registry.get(lang, root).await { Ok(s) => s, Err(e) => return format!("LSP unavailable: {e:#}") };
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            let _ = s.did_open(path, text).await;
            match s.formatting(path).await {
                Ok(edits) if edits.is_empty() => "Already formatted".to_string(),
                Ok(edits) => {
                    let mut out = String::new();
                    for e in edits.iter().take(20) {
                        let preview: String = e.new_text.chars().take(60).collect();
                        out.push_str(&format!("{}:{} -> {}:{}: {preview}\n", e.start_line, e.start_col, e.end_line, e.end_col));
                    }
                    out
                }
                Err(e) => format!("LSP error: {e:#}"),
            }
        }
        ServerState::Unavailable(reason) => format!("unavailable: {reason}"),
        ServerState::Spawning => "still spawning".to_string(),
    }
}

fn format_diags(diags: &[apex_lsp::Diagnostic]) -> String {
    if diags.is_empty() { return "No diagnostics".to_string(); }
    let n_err = diags.iter().filter(|d| d.severity == Some(1)).count();
    let n_warn = diags.iter().filter(|d| d.severity == Some(2)).count();
    let mut out = format!("{n_err} errors, {n_warn} warnings:\n");
    for d in diags.iter().take(20) {
        let sev = match d.severity { Some(1) => "E", Some(2) => "W", _ => "I" };
        out.push_str(&format!("  {sev} {}:{}  {}\n", d.range.0 + 1, d.range.1 + 1, d.message));
    }
    if diags.len() > 20 { out.push_str(&format!("  … {} more\n", diags.len() - 20)); }
    out
}

fn pos_args(args: &Value) -> (usize, usize) {
    (args.get("line").and_then(|l| l.as_u64()).unwrap_or(1) as usize, args.get("col").and_then(|c| c.as_u64()).unwrap_or(1) as usize)
}

fn kind_name(k: u16) -> &'static str {
    match k { 1 => "file", 2 => "module", 3 => "namespace", 4 => "package", 5 => "class", 6 => "method", 7 => "property", 8 => "field", 9 => "constructor", 10 => "enum", 11 => "interface", 12 => "function", 13 => "variable", 14 => "constant", 15 => "string", 23 => "struct", 24 => "event", 25 => "operator", 26 => "type_parameter", _ => "symbol" }
}