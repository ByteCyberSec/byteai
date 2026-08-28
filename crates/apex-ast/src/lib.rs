//! AST-aware code intelligence for ByteAi (APEX) — Phase 2.
//!
//! Tree-sitter based extraction: symbols (functions/classes/structs/impls/
//! imports), whole-definition retrieval for smart reads, and lightweight
//! structural search. Pure local computation — no LSP server required.
//!
//! Design: manual tree traversal with a per-language table of node kinds
//! (no tree-sitter query API — version-tolerant). Cheap: parse is O(n),
//! extraction only visits nodes near the definitions.

use std::path::Path;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based line numbers; 0-based columns.
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Module,
    Constant,
    Type,
    Import,
    Other,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Interface => "interface",
            SymbolKind::Trait => "trait",
            SymbolKind::Module => "module",
            SymbolKind::Constant => "constant",
            SymbolKind::Type => "type",
            SymbolKind::Import => "import",
            SymbolKind::Other => "other",
        }
    }
}

/// Per-language extraction rules.
struct LangRules {
    defs: &'static [(&'static str, SymbolKind)],
    imports: &'static [&'static str],
    name_field: Option<&'static str>,
    name_child_kinds: &'static [&'static str],
}

fn rules_for(lang: &str) -> Option<LangRules> {
    let rules = match lang {
        "rust" => LangRules {
            defs: &[
                ("function_item", SymbolKind::Function),
                ("struct_item", SymbolKind::Struct),
                ("enum_item", SymbolKind::Enum),
                ("impl_item", SymbolKind::Other),
                ("trait_item", SymbolKind::Trait),
                ("mod_item", SymbolKind::Module),
                ("type_item", SymbolKind::Type),
                ("const_item", SymbolKind::Constant),
                ("static_item", SymbolKind::Constant),
                ("macro_definition", SymbolKind::Function),
                ("use_declaration", SymbolKind::Import),
                ("extern_crate_declaration", SymbolKind::Import),
            ],
            imports: &["use_declaration", "extern_crate_declaration"],
            name_field: Some("name"),
            name_child_kinds: &["identifier", "type_identifier", "field_identifier"],
        },
        "python" => LangRules {
            defs: &[
                ("function_definition", SymbolKind::Function),
                ("class_definition", SymbolKind::Class),
                ("decorated_definition", SymbolKind::Other),
                ("import_statement", SymbolKind::Import),
                ("import_from_statement", SymbolKind::Import),
            ],
            imports: &["import_statement", "import_from_statement"],
            name_field: Some("name"),
            name_child_kinds: &["identifier"],
        },
        "typescript" | "javascript" => LangRules {
            defs: &[
                ("function_declaration", SymbolKind::Function),
                ("class_declaration", SymbolKind::Class),
                ("method_definition", SymbolKind::Method),
                ("interface_declaration", SymbolKind::Interface),
                ("type_alias_declaration", SymbolKind::Type),
                ("enum_declaration", SymbolKind::Enum),
                ("import_statement", SymbolKind::Import),
                ("variable_declarator", SymbolKind::Constant),
                ("function_expression", SymbolKind::Function),
                ("arrow_function", SymbolKind::Function),
            ],
            imports: &["import_statement", "import_clause"],
            name_field: Some("name"),
            name_child_kinds: &["identifier", "type_identifier", "property_identifier"],
        },
        "go" => LangRules {
            defs: &[
                ("function_declaration", SymbolKind::Function),
                ("method_declaration", SymbolKind::Method),
                ("type_declaration", SymbolKind::Type),
                ("import_declaration", SymbolKind::Import),
                ("const_declaration", SymbolKind::Constant),
                ("var_declaration", SymbolKind::Constant),
            ],
            imports: &["import_declaration", "import_spec"],
            name_field: Some("name"),
            name_child_kinds: &["identifier", "type_identifier"],
        },
        "c" => LangRules {
            defs: &[
                ("function_definition", SymbolKind::Function),
                ("struct_specifier", SymbolKind::Struct),
                ("enum_specifier", SymbolKind::Enum),
                ("union_specifier", SymbolKind::Struct),
                ("typedef", SymbolKind::Type),
                ("preproc_include", SymbolKind::Import),
            ],
            imports: &["preproc_include"],
            name_field: Some("name"),
            name_child_kinds: &["identifier", "type_identifier", "field_identifier"],
        },
        "cpp" => LangRules {
            defs: &[
                ("function_definition", SymbolKind::Function),
                ("class_specifier", SymbolKind::Class),
                ("struct_specifier", SymbolKind::Struct),
                ("enum_specifier", SymbolKind::Enum),
                ("namespace_definition", SymbolKind::Module),
                ("using_declaration", SymbolKind::Import),
                ("template_declaration", SymbolKind::Other),
                ("preproc_include", SymbolKind::Import),
            ],
            imports: &["preproc_include", "using_declaration"],
            name_field: Some("name"),
            name_child_kinds: &["identifier", "type_identifier", "field_identifier"],
        },
        _ => return None,
    };
    Some(rules)
}

pub fn language_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" => Some("python"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => Some("cpp"),
        "go" => Some("go"),
        _ => None,
    }
}

fn grammar(lang: &str) -> Option<Language> {
    let l = match lang {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    };
    Some(l)
}

/// Parse a source text into a tree for the given language.
pub fn parse(lang: &str, text: &str) -> Result<tree_sitter::Tree> {
    let grammar = grammar(lang).context("unsupported language")?;
    let mut parser = Parser::new();
    parser.set_language(&grammar).context("set language")?;
    parser.parse(text, None).context("parse failed")
}

/// Extract symbols + imports from source text.
pub fn extract(lang: &str, text: &str) -> Result<Vec<Symbol>> {
    let rules = rules_for(lang).context("unsupported language")?;
    let tree = parse(lang, text)?;
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    walk_node(&mut cursor, &rules, text, &mut out);
    Ok(out)
}

/// Extract symbols from a file.
pub fn extract_file(path: &Path) -> Result<Vec<Symbol>> {
    let lang = language_for_path(path).context("unsupported file type")?;
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    extract(lang, &text)
}

/// Extract just the imports from a file (fast path used by smart reads).
pub fn imports_file(path: &Path) -> Result<Vec<String>> {
    let lang = language_for_path(path).context("unsupported file type")?;
    let rules = rules_for(lang).unwrap();
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let tree = parse(lang, &text)?;
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    collect_imports(&mut cursor, &rules, &text, &mut out);
    Ok(out)
}

/// Find a definition by name; returns its full source text (the smart-read win:
/// read just this function/class instead of the whole file).
pub fn find_definition(lang: &str, text: &str, name: &str) -> Option<DefRange> {
    let rules = rules_for(lang)?;
    let tree = parse(lang, text).ok()?;
    let mut found: Option<DefRange> = None;
    let mut cursor = tree.walk();
    let mut search = |c: &mut tree_sitter::TreeCursor| {
        let node = c.node();
        if is_def_node(&node, &rules) && node_name(&node, &rules, text).as_deref() == Some(name) {
            let (sl, sc) = pos(&node.start_position());
            let (el, ec) = pos(&node.end_position());
            let src = node.utf8_text(text.as_bytes()).unwrap_or("").to_string();
            found = Some(DefRange { start_line: sl, start_col: sc, end_line: el, end_col: ec, text: src });
            return true;
        }
        false
    };
    walk_until(&mut cursor, &mut search);
    found
}

#[derive(Debug, Clone)]
pub struct DefRange {
    /// 1-based start line.
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// Full source text of the definition.
    pub text: String,
}

/// Summary lines for a file's symbols — the "read less, understand more" view.
pub fn symbol_summary(path: &Path, max: usize) -> Result<String> {
    let syms = extract_file(path)?;
    let total = syms.len();
    let mut out = String::new();
    let mut shown = 0usize;
    for s in &syms {
        if s.kind == SymbolKind::Import {
            continue; // imports shown separately
        }
        if shown >= max {
            out.push_str(&format!("… {} more symbols\n", total - shown));
            break;
        }
        out.push_str(&format!("{:>6}:{:<4} {:<9} {}\n", s.start_line, s.start_col, s.kind.as_str(), s.name));
        shown += 1;
    }
    Ok(out)
}

// ────────────────────────────────────────────────────────────────────────────
// Traversal helpers
// ────────────────────────────────────────────────────────────────────────────

fn walk_node(cursor: &mut tree_sitter::TreeCursor, rules: &LangRules, text: &str, out: &mut Vec<Symbol>) {
    let node = cursor.node();
    if is_def_node(&node, rules) {
        // Imports: the "name" is the whole statement (e.g. `use std::fmt;`).
        let name = if rules.imports.contains(&node.kind()) {
            node.utf8_text(text.as_bytes())
                .ok()
                .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|t| !t.is_empty())
        } else {
            node_name(&node, rules, text)
        };
        if let Some(name) = name {
            let (sl, sc) = pos(&node.start_position());
            let (el, ec) = pos(&node.end_position());
            let kind = rules
                .defs
                .iter()
                .find(|(k, _)| *k == node.kind())
                .map(|(_, k)| *k)
                .unwrap_or(SymbolKind::Other);
            out.push(Symbol { name, kind, start_line: sl, start_col: sc, end_line: el, end_col: ec });
        }
    }
    if cursor.goto_first_child() {
        loop {
            walk_node(cursor, rules, text, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn collect_imports(cursor: &mut tree_sitter::TreeCursor, rules: &LangRules, text: &str, out: &mut Vec<String>) {
    let node = cursor.node();
    if rules.imports.contains(&node.kind())
        && let Ok(t) = node.utf8_text(text.as_bytes()) {
            let one_line: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
            let clipped: String = one_line.chars().take(160).collect();
            if !out.contains(&clipped) {
                out.push(clipped);
            }
        }
    if cursor.goto_first_child() {
        loop {
            collect_imports(cursor, rules, text, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Walk calling `search` at each node; stops when it returns true.
fn walk_until(cursor: &mut tree_sitter::TreeCursor, search: &mut impl FnMut(&mut tree_sitter::TreeCursor) -> bool) -> bool {
    if search(cursor) {
        return true;
    }
    if cursor.goto_first_child() {
        loop {
            if walk_until(cursor, search) {
                cursor.goto_parent();
                return true;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
    false
}

fn is_def_node(node: &Node, rules: &LangRules) -> bool {
    rules.defs.iter().any(|(k, _)| *k == node.kind())
}

fn node_name(node: &Node, rules: &LangRules, text: &str) -> Option<String> {
    if let Some(field) = rules.name_field
        && let Some(child) = node.child_by_field_name(field)
            && let Ok(t) = child.utf8_text(text.as_bytes()) {
                let t = t.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
    // Bounded-depth search for a name-bearing child (handles wrappers like
    // go type_spec / rust use paths / decorated definitions).
    let mut cursor = node.walk();
    let mut stack = vec![*node];
    for _ in 0..3 {
        let mut next = Vec::new();
        for n in &stack {
            let mut c = n.walk();
            for child in n.children(&mut c) {
                if rules.name_child_kinds.contains(&child.kind())
                    && let Ok(t) = child.utf8_text(text.as_bytes()) {
                        let t = t.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                next.push(child);
            }
        }
        stack = next;
    }
    let _ = &mut cursor;
    None
}

fn pos(p: &tree_sitter::Point) -> (usize, usize) {
    (p.row + 1, p.column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbols() {
        let src = r#"
use std::fmt;

pub struct Point { x: f64, y: f64 }

impl Point {
    pub fn new(x: f64, y: f64) -> Self { Point { x, y } }
    pub fn dist(&self) -> f64 { (self.x * self.x + self.y * self.y).sqrt() }
}

fn main() {
    let p = Point::new(3.0, 4.0);
    println!("{}", p.dist());
}
"#;
        let syms = extract("rust", src).unwrap();
        let names: Vec<(&str, &str)> = syms
            .iter()
            .filter(|s| s.kind != SymbolKind::Import)
            .map(|s| (s.name.as_str(), s.kind.as_str()))
            .collect();
        assert!(names.contains(&("Point", "struct")), "{names:?}");
        assert!(names.contains(&("new", "function")), "{names:?}");
        assert!(names.contains(&("dist", "function")), "{names:?}");
        assert!(names.contains(&("main", "function")), "{names:?}");
        // Point::new line should be 7 (1-based): blank(1) use(2) blank(3)
        // struct(4) blank(5) impl(6) fn new(7)
        let new_sym = syms.iter().find(|s| s.name == "new").unwrap();
        assert!(new_sym.start_line >= 6 && new_sym.start_line <= 8, "line {}", new_sym.start_line);
    }

    #[test]
    fn rust_imports() {
        let src = "use std::fmt;\nuse serde::{Serialize, Deserialize};\n";
        let syms = extract("rust", src).unwrap();
        let imports: Vec<&str> = syms.iter().filter(|s| s.kind == SymbolKind::Import).map(|s| s.name.as_str()).collect();
        assert!(imports.contains(&"use std::fmt;"), "{imports:?}");
        assert!(imports.contains(&"use serde::{Serialize, Deserialize};"), "{imports:?}");
    }

    #[test]
    fn python_symbols() {
        let src = r#"
import os
from pathlib import Path

class Config:
    def __init__(self):
        self.name = "x"

def load(path):
    return Path(path)

def main():
    c = Config()
"#;
        let syms = extract("python", src).unwrap();
        let names: Vec<&str> = syms.iter().filter(|s| s.kind != SymbolKind::Import).map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Config"), "{names:?}");
        assert!(names.contains(&"__init__"), "{names:?}");
        assert!(names.contains(&"load"), "{names:?}");
        assert!(names.contains(&"main"), "{names:?}");
    }

    #[test]
    fn typescript_symbols() {
        let src = r#"
import { readFile } from "fs";

export interface Options { verbose: boolean }

export class Runner {
    run(): void {}
}

export function start(opts: Options): void {}

const helper = () => 42;
"#;
        let syms = extract("typescript", src).unwrap();
        let names: Vec<(&str, &str)> = syms
            .iter()
            .filter(|s| s.kind != SymbolKind::Import)
            .map(|s| (s.name.as_str(), s.kind.as_str()))
            .collect();
        assert!(names.contains(&("Options", "interface")), "{names:?}");
        assert!(names.contains(&("Runner", "class")), "{names:?}");
        assert!(names.contains(&("run", "method")), "{names:?}");
        assert!(names.contains(&("start", "function")), "{names:?}");
        assert!(names.contains(&("helper", "constant")), "{names:?}");
    }

    #[test]
    fn go_symbols() {
        let src = r#"
package main

import "fmt"

type Point struct { X, Y float64 }

func (p Point) Dist() float64 { return p.X }

func main() { fmt.Println(Point{1, 2}.Dist()) }
"#;
        let syms = extract("go", src).unwrap();
        let names: Vec<(&str, &str)> = syms.iter().filter(|s| s.kind != SymbolKind::Import).map(|s| (s.name.as_str(), s.kind.as_str())).collect();
        assert!(names.contains(&("Point", "type")), "{names:?}");
        assert!(names.contains(&("Dist", "method")), "{names:?}");
        assert!(names.contains(&("main", "function")), "{names:?}");
    }

    #[test]
    fn cpp_symbols() {
        let src = r#"
#include <vector>

class Widget {
public:
    void draw();
};

struct Config { int width; };

namespace app {
    int run() { return 0; }
}
"#;
        let syms = extract("cpp", src).unwrap();
        let names: Vec<(&str, &str)> = syms.iter().filter(|s| s.kind != SymbolKind::Import).map(|s| (s.name.as_str(), s.kind.as_str())).collect();
        assert!(names.contains(&("Widget", "class")), "{names:?}");
        assert!(names.contains(&("Config", "struct")), "{names:?}");
        assert!(names.contains(&("run", "function")), "{names:?}");
    }

    #[test]
    fn find_def_returns_text() {
        let src = "fn alpha() { println!(\"a\"); }\nfn beta() { println!(\"b\"); }\n";
        let def = find_definition("rust", src, "beta").unwrap();
        assert!(def.text.contains("fn beta"));
        assert!(def.text.contains("println!"));
        assert_eq!(def.start_line, 2);
    }

    #[test]
    fn find_def_missing() {
        let src = "fn alpha() {}";
        assert!(find_definition("rust", src, "gamma").is_none());
    }

    #[test]
    fn language_map() {
        assert_eq!(language_for_path(Path::new("a.rs")), Some("rust"));
        assert_eq!(language_for_path(Path::new("a.tsx")), Some("typescript"));
        assert_eq!(language_for_path(Path::new("a.py")), Some("python"));
        assert_eq!(language_for_path(Path::new("a.txt")), None);
    }
}
