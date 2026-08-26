//! Graph tool (graphify-inspired). Builds a symbol knowledge graph from the
//! AST extractor: every symbol in a workspace becomes a node with file/line/
//! kind; references between files are edges. Queries: `symbols` (all nodes),
//! `refs <name>` (where a symbol is referenced across the workspace), and
//! `files` (file-level dependency edges via imports).
//!
//! Local, deterministic, no vector store — exactly the graphify thesis.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct GraphTool;

fn walk_files(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 5 || !root.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            // Skip vendored/build/reference dirs — not part of the workspace graph.
            if matches!(name.as_str(), "target" | "research" | "node_modules" | ".git" | "vendor" | "dist" | "build")
                || name.starts_with('.')
            {
                continue;
            }
            walk_files(&p, depth + 1, out);
        } else if let Some(ext) = p.extension().and_then(|x| x.to_str()) {
            if matches!(ext, "rs" | "py" | "ts" | "js" | "go" | "c" | "cpp" | "h") {
                out.push(p);
            }
        }
    }
}

#[derive(Default)]
struct Node {
    name: String,
    kind: String,
    file: String,
    line: usize,
}

impl GraphTool {
    /// Collect all symbols in a workspace: (name, kind, file, line).
    fn collect(root: &Path) -> Vec<Node> {
        let mut files = Vec::new();
        walk_files(root, 0, &mut files);
        let mut nodes = Vec::new();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let Some(lang) = apex_ast::language_for_path(&f) else { continue };
            let Ok(syms) = apex_ast::extract(lang, &text) else { continue };
            for s in syms {
                if matches!(s.kind, apex_ast::SymbolKind::Import) {
                    continue;
                }
                nodes.push(Node {
                    name: s.name.clone(),
                    kind: s.kind.as_str().to_string(),
                    file: f.display().to_string(),
                    line: s.start_line,
                });
            }
        }
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        nodes
    }

    /// Where is `name` referenced? Lines containing the symbol in files that
    /// also define other symbols (cheap approximation of reference edges).
    fn references(root: &Path, name: &str, nodes: &[Node]) -> Vec<(String, usize, String)> {
        let mut files = Vec::new();
        walk_files(root, 0, &mut files);
        let mut out = Vec::new();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            // Skip files that define the symbol itself (that's the definition site).
            if nodes.iter().any(|n| n.name == name && n.file == f.display().to_string()) {
                continue;
            }
            for (i, line) in text.lines().enumerate() {
                if line.contains(name) {
                    let trimmed: String = line.trim().chars().take(100).collect();
                    out.push((f.display().to_string(), i + 1, trimmed));
                    if out.len() >= 30 {
                        return out;
                    }
                }
            }
        }
        out
    }
}

impl Tool for GraphTool {
    fn name(&self) -> &'static str {
        "graph"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "graph".into(),
            description: "Code knowledge graph via AST (graphify pattern). Actions: \
symbols (all symbols: name/kind/file/line), refs <name> (where a symbol is used), \
files (file dependency edges via imports). Local, deterministic, no vector store. \
Use to understand what depends on what before refactoring.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["symbols", "refs", "files"] },
                    "name": { "type": "string", "description": "Symbol name for refs action" },
                    "path": { "type": "string", "description": "Root directory (default .)" },
                    "filter": { "type": "string", "description": "Show only symbols whose name contains this (optional)" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("symbols").to_string();
            let root = PathBuf::from(args.get("path").and_then(|p| p.as_str()).unwrap_or("."));

            match action.as_str() {
                "symbols" => {
                    let nodes = Self::collect(&root);
                    if nodes.is_empty() {
                        return ok_outcome("", "graph", format!("no symbols found under {}", root.display()), started.elapsed().as_millis() as u64);
                    }
                    let filter = args.get("filter").and_then(|f| f.as_str()).unwrap_or("").to_string();
                    let mut out = String::new();
                    out.push_str(&format!("{} symbol(s) under {}\n", nodes.len(), root.display()));
                    let mut shown = 0usize;
                    for n in &nodes {
                        if !filter.is_empty() && !n.name.contains(&filter) {
                            continue;
                        }
                        if shown >= 80 {
                            out.push_str(&format!("... {} more (add filter=... to narrow)\n", nodes.len() - shown));
                            break;
                        }
                        out.push_str(&format!("  {:<28} {:<10} {}:{}\n", n.name, n.kind, n.file, n.line));
                        shown += 1;
                    }
                    if shown == 0 {
                        out.push_str(&format!("  (nothing matches filter {filter:?})\n"));
                    }
                    ok_outcome("", "graph", out, started.elapsed().as_millis() as u64)
                }
                "refs" => {
                    let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    if name.is_empty() {
                        return ok_outcome("", "graph", "ERROR: `name` required for refs".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let nodes = Self::collect(&root);
                    let refs = Self::references(&root, &name, &nodes);
                    let mut out = String::new();
                    let defs: Vec<_> = nodes.iter().filter(|n| n.name == name).collect();
                    out.push_str(&format!("definitions of {name:?}:\n"));
                    for d in &defs {
                        out.push_str(&format!("  {}:{} ({})\n", d.file, d.line, d.kind));
                    }
                    out.push_str(&format!("references ({}) elsewhere:\n", refs.len()));
                    for (f, l, snippet) in &refs {
                        out.push_str(&format!("  {f}:{l}  {snippet}\n"));
                    }
                    if refs.is_empty() {
                        out.push_str("  (none)\n");
                    }
                    ok_outcome("", "graph", out, started.elapsed().as_millis() as u64)
                }
                "files" => {
                    // File-level dependency edges: which files import which.
                    let mut files = Vec::new();
                    walk_files(&root, 0, &mut files);
                    let mut edges: Vec<(String, String)> = Vec::new();
                    let mut file_imports: HashMap<String, Vec<String>> = HashMap::new();
                    for f in &files {
                        let Ok(_) = std::fs::read_to_string(f) else { continue };
                        let Some(_) = apex_ast::language_for_path(f) else { continue };
                        let Ok(imports) = apex_ast::imports_file(f) else { continue };
                        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
                        file_imports.insert(rel.clone(), imports.clone());
                        for imp in imports {
                            // crude module→file resolution: last path component
                            // (strip `::`-suffixes like `::{ToolDef}` first)
                            let base = imp.split("::").next().unwrap_or(&imp);
                            let module = if base.is_empty() { imp.clone() } else { base.to_string() };
                            let target: Vec<_> = files.iter()
                                .filter(|cand| {
                                    cand.file_name().map(|n| n.to_string_lossy().starts_with(&module)).unwrap_or(false)
                                })
                                .map(|cand| cand.strip_prefix(&root).unwrap_or(cand).display().to_string())
                                .collect();
                            for t in target {
                                if t != rel {
                                    edges.push((rel.clone(), t));
                                }
                            }
                        }
                    }
                    let mut out = String::new();
                    out.push_str(&format!("{} files, {} import edges\n", files.len(), edges.len()));
                    for (a, b) in edges {
                        out.push_str(&format!("  {a} -> {b}\n"));
                    }
                    ok_outcome("", "graph", out, started.elapsed().as_millis() as u64)
                }
                other => ok_outcome("", "graph", format!("ERROR: unknown action {other:?}"), started.elapsed().as_millis() as u64),
            }
        })
    }
}
