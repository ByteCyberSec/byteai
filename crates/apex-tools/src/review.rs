//! Review tool (Phase 9): independent verification agent.
//!
//! After a task is "done", run an independent review pass: typecheck/test via
//! the verify gate, LSP diagnostics on changed files, and a structural sanity
//! check (balanced braces, no TODO/FIXME left, no obviously-broken markers).
//! The reviewer's verdict is PASS / FAIL + findings. This is the independent
//! second-opinion step that keeps the primary agent honest.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apex_lsp::LspRegistry;
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{BoxFuture, Tool, ok_outcome};

const TIMEOUT: Duration = Duration::from_secs(120);

pub struct ReviewTool {
    lsp: Option<Arc<LspRegistry>>,
}

impl ReviewTool {
    pub fn new(lsp: Option<Arc<LspRegistry>>) -> Self {
        Self { lsp }
    }
}

impl Default for ReviewTool {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Tool for ReviewTool {
    fn name(&self) -> &'static str {
        "review"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "review".into(),
            description: "Independent review pass before declaring a task done. Runs the \
project's tests/typecheck, LSP diagnostics on listed files, and structural checks \
(balanced delimiters, no leftover TODO/FIXME, no debug printlns in Rust src). \
Verdict: PASS or FAIL with findings. Use this AFTER the edit loop, BEFORE the final report.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project directory (default: .)" },
                    "files": { "type": "array", "items": { "type": "string" }, "description": "Files changed by the task; diagnostics + structural checks run on these." }
                },
                "required": []
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let started = Instant::now();
            let dir = args.get("path").and_then(|p| p.as_str()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            let files: Vec<String> = args.get("files").and_then(|f| f.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();

            let mut findings: Vec<String> = Vec::new();
            let mut checks_ok = 0u32;
            let mut checks_fail = 0u32;

            // 1. Structural checks on changed files.
            for f in &files {
                let p = PathBuf::from(f);
                if !p.exists() {
                    findings.push(format!("MISSING: {f} does not exist"));
                    checks_fail += 1;
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else {
                    findings.push(format!("UNREADABLE: {f}"));
                    checks_fail += 1;
                    continue;
                };
                // Balanced delimiters (rough but catches obvious breakage).
                for (open, close, label) in [('{', '}', "braces"), ('(', ')', "parens"), ('[', ']', "brackets")] {
                    let o = text.chars().filter(|c| *c == open).count();
                    let c = text.chars().filter(|c| *c == close).count();
                    if o != c {
                        findings.push(format!("UNBALANCED {label}: {f} ({o} {open} vs {c} {close})"));
                        checks_fail += 1;
                    }
                }
                // Leftover markers.
                for marker in ["TODO", "FIXME", "XXX"] {
                    if text.contains(marker) {
                        findings.push(format!("MARKER {marker}: {f}"));
                        checks_fail += 1;
                    }
                }
                // Rust debug printlns.
                if f.ends_with(".rs") && text.lines().any(|l| l.trim_start().starts_with("println!")) {
                    findings.push(format!("DEBUG-PRINTLN: {f} (left in source)"));
                    checks_fail += 1;
                }
                // Ponytail/YAGNI: over-engineering heuristics (advisory — don't block ship).
                if f.ends_with(".rs") {
                    let trait_count = text.matches("trait ").count();
                    let impl_count = text.matches("impl ").count();
                    if trait_count >= 1 && impl_count == 1 {
                        findings.push(format!("YAGNI(advisory): {f} defines a trait but only one impl — premature abstraction?"));
                    }
                    if text.contains("Box<dyn ") && text.matches("Box<dyn ").count() >= 2 && text.matches("struct ").count() <= 2 {
                        findings.push(format!("YAGNI(advisory): {f} uses Box<dyn with few structs — premature abstraction?"));
                    }
                    for line in text.lines() {
                        if line.contains("fn ") && line.matches(',').count() > 20 {
                            findings.push(format!("YAGNI(advisory): {f} has a function with many params — simplify?"));
                            break;
                        }
                    }
                }
                checks_ok += 1;
            }

            // 2. Typecheck/test via the verify machinery (reuse commands here to stay lean).
            if dir.join("Cargo.toml").exists() {
                match tokio::time::timeout(TIMEOUT, Command::new("cargo").args(["check", "--quiet"]).current_dir(&dir).output()).await {
                    Ok(Ok(out)) if out.status.success() => checks_ok += 1,
                    Ok(Ok(out)) => {
                        let err: String = String::from_utf8_lossy(&out.stderr).lines().take(6).collect::<Vec<_>>().join("\n");
                        findings.push(format!("CARGO CHECK FAILED:\n{err}"));
                        checks_fail += 1;
                    }
                    Ok(Err(e)) => { findings.push(format!("cargo check error: {e}")); checks_fail += 1; }
                    Err(_) => { findings.push("cargo check timed out".into()); checks_fail += 1; }
                }
            } else if dir.join("package.json").exists() {
                match tokio::time::timeout(TIMEOUT, Command::new("npm").args(["test", "--silent"]).current_dir(&dir).output()).await {
                    Ok(Ok(out)) if out.status.success() => checks_ok += 1,
                    Ok(Ok(out)) => {
                        let err: String = String::from_utf8_lossy(&out.stderr).lines().take(6).collect::<Vec<_>>().join("\n");
                        findings.push(format!("NPM TEST FAILED:\n{err}"));
                        checks_fail += 1;
                    }
                    _ => { findings.push("npm test failed/timed out".into()); checks_fail += 1; }
                }
            }

            // 3. LSP diagnostics on changed files.
            if let Some(registry) = &lsp {
                for f in &files {
                    let p = PathBuf::from(f);
                    if !p.exists() {
                        continue;
                    }
                    let Some(lang) = apex_lsp::language_for_path(&p) else { continue };
                    if !registry.supports(&lang) {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(&p) else { continue };
                    let root = p.parent().map(|x| x.to_path_buf()).unwrap_or_default();
                    let Ok(state) = registry.get(&lang, &root).await else { continue };
                    let mut st = state.lock().await;
                    if let apex_lsp::ServerState::Ready(s) = &mut *st {
                        let _ = s.did_open(&p, &text).await;
                        let _ = s.did_change(&p, &text, 1).await;
                        let diags = s.wait_diagnostics(&p, Duration::from_secs(6)).await;
                        let errs: Vec<_> = diags.iter().filter(|d| d.severity == Some(1)).collect();
                        if !errs.is_empty() {
                            for d in errs.iter().take(3) {
                                findings.push(format!("DIAG {f}:{}:{} {}", d.range.0 + 1, d.range.1 + 1, d.message));
                            }
                            checks_fail += 1;
                        } else {
                            checks_ok += 1;
                        }
                    }
                }
            }

            // Verdict.
            let verdict = if checks_fail == 0 { "PASS" } else { "FAIL" };
            let mut out = String::new();
            out.push_str(&format!("REVIEW {verdict} — {checks_ok} ok, {checks_fail} failing\n"));
            for f in &findings {
                out.push_str(&format!("  - {f}\n"));
            }
            if checks_fail == 0 {
                out.push_str("Independent review complete: safe to ship.\n");
            } else {
                out.push_str("Fix the findings above, then re-review before declaring done.\n");
            }
            ok_outcome("", "review", out, started.elapsed().as_millis() as u64)
        })
    }
}
