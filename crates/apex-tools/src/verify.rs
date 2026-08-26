//! Verify tool: test detection, typecheck, LSP diagnostics, verification gate.
//!
//! Detects the project kind (Cargo/npm/pyproject/go), runs the appropriate
//! checks with timeouts, and reports a structured pass/fail summary the agent
//! uses as its completion gate. Optional LSP diagnostics round out the report.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apex_lsp::LspRegistry;
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{BoxFuture, Tool, ok_outcome};

const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
struct CheckResult {
    name: String,
    ok: bool,
    detail: String,
    elapsed_ms: u64,
}

impl CheckResult {
    fn ok(name: &str, detail: String, elapsed_ms: u64) -> Self {
        Self { name: name.into(), ok: true, detail, elapsed_ms }
    }
    fn fail(name: &str, detail: String, elapsed_ms: u64) -> Self {
        Self { name: name.into(), ok: false, detail, elapsed_ms }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ProjectKind {
    Cargo,
    Node,
    Python,
    Go,
    Unknown,
}

fn detect_project(dir: &PathBuf) -> ProjectKind {
    if dir.join("Cargo.toml").exists() {
        ProjectKind::Cargo
    } else if dir.join("package.json").exists() {
        ProjectKind::Node
    } else if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() || dir.join("setup.py").exists() {
        ProjectKind::Python
    } else if dir.join("go.mod").exists() {
        ProjectKind::Go
    } else {
        ProjectKind::Unknown
    }
}

async fn run_check(dir: &PathBuf, name: &str, cmd: &str, args: &[&str]) -> CheckResult {
    let started = Instant::now();
    let output = tokio::time::timeout(
        CHECK_TIMEOUT,
        Command::new(cmd).args(args).current_dir(dir).output(),
    )
    .await;
    let elapsed = started.elapsed().as_millis() as u64;
    match output {
        Ok(Ok(out)) => {
            let status = out.status;
            let detail;
            if status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let tail: String = stdout.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                detail = format!("exit 0\n{tail}");
                CheckResult::ok(name, detail, elapsed)
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let err_tail: String = stderr.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                detail = format!("exit {}\n{}", status.code().unwrap_or(-1), err_tail);
                CheckResult::fail(name, detail, elapsed)
            }
        }
        Ok(Err(e)) => CheckResult::fail(name, format!("failed to run {cmd}: {e}"), elapsed),
        Err(_) => CheckResult::fail(name, format!("timed out after {}s", CHECK_TIMEOUT.as_secs()), elapsed),
    }
}

pub struct VerifyTool {
    lsp: Option<Arc<LspRegistry>>,
}

impl VerifyTool {
    pub fn new(lsp: Option<Arc<LspRegistry>>) -> Self {
        Self { lsp }
    }
}

impl Default for VerifyTool {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Tool for VerifyTool {
    fn name(&self) -> &'static str {
        "verify"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "verify".into(),
            description: "Verification gate: run the project's tests and typecheck (auto-detected: \
cargo/npm/pytest/go), optionally collect LSP diagnostics. Use BEFORE declaring a task done. \
Returns structured PASS/FAIL per check.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project directory (default: current directory)." },
                    "checks": { "type": "array", "items": { "type": "string", "enum": ["test", "typecheck", "diagnostics"] }, "description": "Which checks to run (default: test + typecheck + diagnostics)." },
                    "files": { "type": "array", "items": { "type": "string" }, "description": "Files for LSP diagnostics (default: changed/error files)." }
                },
                "required": []
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let started = Instant::now();
            let dir = args
                .get("path")
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let checks: Vec<String> = args
                .get("checks")
                .and_then(|c| c.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec!["test".into(), "typecheck".into(), "diagnostics".into()]);

            let kind = detect_project(&dir);
            let mut results: Vec<CheckResult> = Vec::new();

            if checks.iter().any(|c| c == "test") {
                let r = match kind {
                    ProjectKind::Cargo => run_check(&dir, "cargo test", "cargo", &["test", "--quiet"]).await,
                    ProjectKind::Node => run_check(&dir, "npm test", "npm", &["test", "--silent"]).await,
                    ProjectKind::Python => run_check(&dir, "pytest", "python3", &["-m", "pytest", "-q"]).await,
                    ProjectKind::Go => run_check(&dir, "go test", "go", &["test", "./..."]).await,
                    ProjectKind::Unknown => CheckResult::fail("test", "no supported project detected (Cargo.toml/package.json/pyproject/go.mod)".to_string(), 0),
                };
                results.push(r);
            }

            if checks.iter().any(|c| c == "typecheck") {
                let r = match kind {
                    ProjectKind::Cargo => run_check(&dir, "cargo check", "cargo", &["check", "--quiet"]).await,
                    ProjectKind::Node => run_check(&dir, "tsc --noEmit", "npx", &["tsc", "--noEmit"]).await,
                    ProjectKind::Python => run_check(&dir, "mypy", "python3", &["-m", "mypy", "."]).await,
                    ProjectKind::Go => run_check(&dir, "go vet", "go", &["vet", "./..."]).await,
                    ProjectKind::Unknown => CheckResult::fail("typecheck", "no supported project detected".to_string(), 0),
                };
                results.push(r);
            }

            if checks.iter().any(|c| c == "diagnostics") {
                if let Some(registry) = &lsp {
                    let files: Vec<String> = args
                        .get("files")
                        .and_then(|f| f.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let mut diag_out = String::new();
                    let mut n_err = 0usize;
                    let mut n_warn = 0usize;
                    for f in files {
                        let p = PathBuf::from(&f);
                        if !p.exists() {
                            continue;
                        }
                        let lang = match apex_lsp::language_for_path(&p) {
                            Some(l) => l,
                            None => continue,
                        };
                        if !registry.supports(&lang) {
                            continue;
                        }
                        let Ok(text) = std::fs::read_to_string(&p) else { continue };
                        let root = p.parent().map(|x| x.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
                        let state = match registry.get(&lang, &root).await {
                            Ok(s) => s,
                            Err(_) => continue,
                        };
                        let mut st = state.lock().await;
                        if let apex_lsp::ServerState::Ready(s) = &mut *st {
                            let _ = s.did_open(&p, &text).await;
                            let _ = s.did_change(&p, &text, 1).await;
                            let diags = s.wait_diagnostics(&p, Duration::from_secs(6)).await;
                            drop(st);
                            let e = diags.iter().filter(|d| d.severity == Some(1)).count();
                            let w = diags.iter().filter(|d| d.severity == Some(2)).count();
                            n_err += e;
                            n_warn += w;
                            for d in diags.iter().take(5) {
                                let sev = match d.severity { Some(1) => "E", Some(2) => "W", _ => "I" };
                                diag_out.push_str(&format!("  {sev} {f}:{}:{}  {}\n", d.range.0 + 1, d.range.1 + 1, d.message));
                            }
                        }
                    }
                    let ok = n_err == 0;
                    let mut detail = format!("{n_err} errors, {n_warn} warnings");
                    if !diag_out.is_empty() {
                        detail.push('\n');
                        detail.push_str(&diag_out);
                    }
                    results.push(if ok {
                        CheckResult::ok("lsp diagnostics", detail, started.elapsed().as_millis() as u64)
                    } else {
                        CheckResult::fail("lsp diagnostics", detail, started.elapsed().as_millis() as u64)
                    });
                } else {
                    results.push(CheckResult::fail("lsp diagnostics", "LSP registry not configured".to_string(), 0));
                }
            }

            // ── Summary ───────────────────────────────────────────────────────
            let n_fail = results.iter().filter(|r| !r.ok).count();
            let mut out = String::new();
            out.push_str(&format!("verify {} — {} check(s), {} passed, {} failed\n", kind_label(&kind), results.len(), results.len() - n_fail, n_fail));
            for r in &results {
                out.push_str(&format!("  [{}] {} ({:?} ms)\n", if r.ok { "PASS" } else { "FAIL" }, r.name, r.elapsed_ms));
                if !r.ok || r.name == "test" {
                    // Show detail for failures and always for tests (assertion output is informative).
                    for line in r.detail.lines().take(8) {
                        out.push_str(&format!("      {line}\n"));
                    }
                }
            }
            if n_fail > 0 {
                out.push_str("GATE: FAIL — do NOT declare the task done. Fix the failures and re-verify.\n");
            } else {
                out.push_str("GATE: PASS — safe to declare the task done.\n");
            }
            ok_outcome("", "verify", out, started.elapsed().as_millis() as u64)
        })
    }
}

fn kind_label(k: &ProjectKind) -> &'static str {
    match k {
        ProjectKind::Cargo => "rust/cargo",
        ProjectKind::Node => "node/npm",
        ProjectKind::Python => "python",
        ProjectKind::Go => "go",
        ProjectKind::Unknown => "unknown",
    }
}
