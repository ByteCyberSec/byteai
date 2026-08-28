//! `gates` — acceptance ledger (unlazy-style GATES.md), native Rust.
//!
//! The core unlazy insight: prose cannot enforce prose. A model that
//! under-executes instructions also under-executes the instruction not to
//! under-execute. So completion is proven against a machine-checked ledger,
//! not declared. This tool is a faithful port of the unlazy gate format:
//!
//! ```markdown
//! # Gates: <task>
//!
//! OWNS: src/import/**, tests/import/**
//!
//! - [ ] G1: valid fixture imports completely
//!   CHECK: node scripts/check-import.mjs fixtures/valid.json
//!   EXPECT: import verification passed
//!   EVIDENCE: pending
//! ```
//!
//! A runnable gate passes only when its CHECK process exits 0 AND EXPECT
//! matches combined stdout+stderr. A checked box with `EVIDENCE: pending`
//! still counts as unmet. Actions:
//!   * status   — parse and report states, never execute or write
//!   * run      — execute unmet runnable gates, update the ledger
//!   * reverify — re-run EVERY runnable gate (parent re-verification);
//!     stale failures are demoted back to unmet
//!   * create   — scaffold a ledger from a task + gate list

use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use regex::Regex;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{BoxFuture, Tool, ok_outcome};

const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, Clone)]
struct Gate {
    checked: bool,
    id: String,
    title: String,
    check: Option<String>,
    expect: Option<String>,
    evidence: Option<String>,
    cwd: Option<String>,
    line_box: usize,        // index of the "- [ ] G1:" line
    line_evidence: Option<usize>, // index of the "  EVIDENCE:" line
    abandoned: bool,
}

#[derive(Debug, Default)]
struct Ledger {
    gates: Vec<Gate>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// Parse a GATES.md ledger. Ignores fenced code blocks. Does not execute.
fn parse(text: &str) -> Ledger {
    let mut ledger = Ledger::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut cur: Option<usize> = None; // index into ledger.gates
    let mut in_fence: Option<&str> = None;
    let mut saw_gate = false;

    for (i, raw) in lines.iter().enumerate() {
        let line = *raw;
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();

        // Fenced code blocks (CommonMark-ish): ``` or ~~~, >= opener length.
        if let Some(f) = in_fence {
            if trimmed.starts_with(f) && trimmed.len() >= f.len() {
                in_fence = None;
            }
            continue;
        }
        if (trimmed.starts_with("```") || trimmed.starts_with("~~~"))
            && trimmed[3..].trim_start().is_empty()
        {
            in_fence = Some(&trimmed[..3]);
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = Some(&trimmed[..3]);
            continue;
        }

        // ABANDON: <id> <reason> at column 1 (file-level statement).
        if trimmed.starts_with("ABANDON:") && indent == 0 {
            let rest = &trimmed["ABANDON:".len()..].trim();
            if let Some((id, reason)) = rest.split_once(char::is_whitespace) {
                let id = id.trim();
                let reason = reason.trim();
                if let Some(g) = ledger.gates.iter_mut().find(|g| g.id == id) {
                    g.abandoned = true;
                    if reason.is_empty() {
                        ledger.errors.push(format!("line {}: ABANDON needs a non-empty reason", i + 1));
                    }
                } else {
                    ledger.errors.push(format!("line {}: ABANDON names unknown gate {:?}", i + 1, id));
                }
            } else {
                ledger.errors.push(format!("line {}: ABANDON needs '<id> <reason>'", i + 1));
            }
            continue;
        }

        // Gate start: "- [ ] ID: title" or "- [x] ID: title"
        if trimmed.starts_with("- [")
            && let Some(rest) = trimmed.strip_prefix("- [") {
                // "- [ ] ID:" leaves " ] ID:" (leading space); "- [x] ID:" leaves "x] ID:".
                let (checked, after) = if let Some(r) = rest.strip_prefix("] ") {
                    (false, r)
                } else if let Some(r) = rest.strip_prefix(" ] ") {
                    (false, r)
                } else if let Some(r) = rest.strip_prefix("x] ") {
                    (true, r)
                } else {
                    ledger.errors.push(format!("line {}: malformed gate marker", i + 1));
                    continue;
                };
                if let Some((id, title)) = after.split_once(':') {
                    let id = id.trim().to_string();
                    let title = title.trim().to_string();
                    if id.is_empty() {
                        ledger.errors.push(format!("line {}: gate needs an explicit non-empty id", i + 1));
                    }
                    if ledger.gates.iter().any(|g| g.id == id) {
                        ledger.errors.push(format!("line {}: duplicate gate id {:?}", i + 1, id));
                    }
                    ledger.gates.push(Gate {
                        checked,
                        id,
                        title,
                        check: None,
                        expect: None,
                        evidence: None,
                        cwd: None,
                        line_box: i,
                        line_evidence: None,
                        abandoned: false,
                    });
                    cur = Some(ledger.gates.len() - 1);
                    saw_gate = true;
                    continue;
                }
            }

        // Attributes under a gate: CHECK / EXPECT / EVIDENCE / CWD (indented).
        if let Some(g) = cur.and_then(|c| ledger.gates.get_mut(c)) {
            for (key, val) in [
                ("CHECK:", &mut g.check),
                ("EXPECT:", &mut g.expect),
                ("EVIDENCE:", &mut g.evidence),
                ("CWD:", &mut g.cwd),
            ] {
                if let Some(v) = trimmed.strip_prefix(key) {
                    if indent == 0 {
                        ledger.errors.push(format!("line {}: {key} must be indented under its gate", i + 1));
                    } else {
                        *val = Some(v.trim().to_string());
                        if key == "EVIDENCE:" {
                            g.line_evidence = Some(i);
                        }
                    }
                    break;
                }
            }
        }
    }

    if !saw_gate {
        ledger.errors.push("ledger defines zero gates".to_string());
    }
    // A runnable gate needs both CHECK and EXPECT; a manual gate needs neither.
    for g in &ledger.gates {
        match (&g.check, &g.expect) {
            (Some(_), None) => ledger.errors.push(format!("gate {} has CHECK but no EXPECT", g.id)),
            (None, Some(_)) => ledger.errors.push(format!("gate {} has EXPECT but no CHECK", g.id)),
            _ => {}
        }
        // Validate EXPECT regex syntax if slash-wrapped.
        if let Some(e) = &g.expect
            && let Some(pat) = expect_regex(e)
                && Regex::new(pat).is_err() {
                    ledger.errors.push(format!("gate {} has an invalid EXPECT regex {:?}", g.id, e));
                }
        if g.evidence.as_deref().map(|e| e == "pending").unwrap_or(false) && g.checked {
            ledger.warnings.push(format!("gate {} is checked but evidence is pending", g.id));
        }
    }
    ledger
}

/// If EXPECT is `/pattern/` (optionally `/pattern/i`), return the inner
/// pattern for regex matching; otherwise return None (plain substring).
fn expect_regex(expect: &str) -> Option<&str> {
    let e = expect.trim();
    if let Some(body) = e.strip_prefix('/') {
        // trailing slash, optional flags
        if let Some(slash) = body.rfind('/') {
            let flags = &body[slash + 1..];
            if flags.is_empty() || flags == "i" {
                return Some(&body[..slash]);
            }
        }
    }
    None
}

/// Check whether `output` satisfies the EXPECT clause.
fn expect_matches(expect: &str, output: &str) -> bool {
    match expect_regex(expect) {
        Some(pat) => {
            let flags = expect.trim();
            let ci = flags.ends_with('/') || flags.ends_with("/i");
            let re = if ci { Regex::new(&format!("(?i){pat}")) } else { Regex::new(pat) };
            match re {
                Ok(re) => re.is_match(output),
                Err(_) => output.contains(expect),
            }
        }
        None => output.contains(expect),
    }
}

/// Execute one CHECK command in the given cwd; return (exit_ok, combined).
async fn run_check(check: &str, cwd: &str) -> (bool, String) {
    let result = timeout(CHECK_TIMEOUT, async {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(check);
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
        cmd.output().await
    })
    .await;
    match result {
        Ok(Ok(out)) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.stderr.is_empty() {
                combined.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            (out.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("IO error: {e:#}")),
        Err(_) => (false, format!("TIMEOUT after {}s", CHECK_TIMEOUT.as_secs())),
    }
}

/// Render the ledger back to a string, applying the passed line updates.
fn render(lines: &[&str], updates: &[(usize, String)]) -> String {
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    for (idx, text) in updates {
        if *idx < out.len() {
            out[*idx] = text.clone();
        }
    }
    out.join("\n") + "\n"
}

/// Main action runner: status / run / reverify / create.
async fn run_action(action: &str, path: &str, gates: Option<&Vec<String>>) -> ToolOutcome {
    let started = Instant::now();
    let mut out = String::new();
    let elapsed = || started.elapsed().as_millis() as u64;

    if action == "create" {
        let task = gates.and_then(|g| g.first()).cloned().unwrap_or_else(|| "untitled".into());
        let ledger_text = format!(
            "# Gates: {task}\n\nOWNS: <repository-relative globs this leaf may write>\n\nScope: <one sentence: the complete deliverable>\n\n"
        );
        let template = match std::fs::write(path, ledger_text) {
            Ok(()) => format!("created ledger at {path}\n"),
            Err(e) => return ok_outcome("", "gates", format!("ERROR: {e:#}\n"), elapsed()),
        };
        out.push_str(&template);
        out.push_str("Add gates as:\n  - [ ] G1: <observable outcome>\n    CHECK: <command>\n    EXPECT: <success marker>\n    EVIDENCE: pending\n");
        return ok_outcome("", "gates", out, elapsed());
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return ok_outcome("", "gates", format!("ERROR: cannot read {path}: {e:#}\n"), elapsed()),
    };
    let ledger = parse(&text);
    if !ledger.errors.is_empty() {
        for e in &ledger.errors {
            out.push_str(&format!("  ERROR: {e}\n"));
        }
        return ok_outcome("", "gates", out, elapsed());
    }

    let lines: Vec<&str> = text.lines().collect();
        let ledger_parent = std::path::Path::new(path).parent().and_then(|p| p.to_str()).unwrap_or(".");

    if action == "status" {
        let (mut met, mut unmet, mut abandoned) = (0, 0, 0);
        for g in &ledger.gates {
            let state = if g.abandoned {
                abandoned += 1;
                "ABANDONED"
            } else if g.checked && g.evidence.as_deref().map(|e| e != "pending").unwrap_or(false) {
                met += 1;
                "met"
            } else {
                unmet += 1;
                "unmet"
            };
            out.push_str(&format!("  {state:9} {}: {}\n", g.id, g.title));
            if let Some(c) = &g.check {
                out.push_str(&format!("    CHECK: {c}\n"));
            }
        }
        out.push_str(&format!(
            "\n  {met} met, {unmet} unmet, {abandoned} abandoned ({} gates)\n",
            ledger.gates.len()
        ));
        return ok_outcome("", "gates", out, elapsed());
    }

    // run / reverify
    let reverify = action == "reverify";
    let mut updates: Vec<(usize, String)> = Vec::new();
    let mut met = 0;
    let mut unmet = 0;
    let mut abandoned = 0;
    for g in &ledger.gates {
        if g.abandoned {
            abandoned += 1;
            out.push_str(&format!("  ABANDONED {}: {} (handoff required)\n", g.id, g.title));
            continue;
        }
        let is_runnable = g.check.is_some();
        let should_run = is_runnable && (!g.checked || reverify);
        if !is_runnable {
            // Manual gate: met only if checked with recorded evidence.
            if g.checked && g.evidence.as_deref().map(|e| e != "pending").unwrap_or(false) {
                met += 1;
            } else {
                unmet += 1;
                out.push_str(&format!("  UNMET {} (manual, no evidence): {}\n", g.id, g.title));
            }
            continue;
        }
        if !should_run {
            // Already met and not reverifying.
            met += 1;
            continue;
        }
        let (exit_ok, output) = run_check(g.check.as_deref().unwrap_or("true"), g.cwd.as_deref().unwrap_or(ledger_parent)).await;
        let matched = g.expect.as_ref().map(|e| expect_matches(e, &output)).unwrap_or(false);
        let passed = exit_ok && matched;
        if passed {
            met += 1;
            updates.push((g.line_box, format!("- [x] {}: {}", g.id, g.title)));
            let evidence = format!("exit 0 · {}", output.lines().next().unwrap_or("").trim().chars().take(120).collect::<String>());
            if let Some(li) = g.line_evidence {
                updates.push((li, format!("  EVIDENCE: {evidence}")));
            }
            out.push_str(&format!("  ✓ {}: {}\n", g.id, g.title));
        } else {
            unmet += 1;
            let why = if !exit_ok { "exit != 0" } else { "EXPECT not matched" };
            out.push_str(&format!("  ✗ {}: {} ({why})\n", g.id, g.title));
            if let Some(li) = g.line_evidence {
                updates.push((li, format!("  EVIDENCE: pending ({why})")));
            }
        }
    }

    if !updates.is_empty() {
        let rendered = render(&lines, &updates);
        if let Err(e) = std::fs::write(path, rendered) {
            out.push_str(&format!("  ERROR: cannot write {path}: {e:#}\n"));
        }
    }

    out.push_str(&format!(
        "\n  {met} met, {unmet} unmet, {abandoned} abandoned{}\n",
        if reverify { " (reverified)" } else { "" }
    ));
    ok_outcome("", "gates", out, elapsed())
}

pub struct GatesTool;

impl Tool for GatesTool {
    fn name(&self) -> &'static str {
        "gates"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "gates".into(),
            description: "unlazy-style acceptance ledger: prove completion against a GATES.md file, not by declaration. Actions: status (report only), run (execute unmet CHECKs, flip boxes only when EXPECT matches), reverify (re-run ALL gates — parent re-verification), create (scaffold a ledger). Input: {action, path, gates?}. A gate passes only when its CHECK exits 0 AND EXPECT matches output; checked boxes with EVIDENCE: pending still count as unmet.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["status", "run", "reverify", "create"], "description": "What to do. status never executes or writes; reverify re-runs even met gates." },
                    "path": { "type": "string", "description": "Path to GATES.md (default GATES.md)" },
                    "gates": { "type": "array", "items": { "type": "string" }, "description": "For create: [task_title] or a list of 'G1: title | CHECK: cmd | EXPECT: marker' gate specs" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let action = args.get("action").and_then(Value::as_str).unwrap_or("status").to_string();
            let path = args.get("path").and_then(Value::as_str).unwrap_or("GATES.md").to_string();
            let gates: Option<Vec<String>> = args
                .get("gates")
                .and_then(|g| g.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect());
            run_action(&action, &path, gates.as_ref()).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ledger() -> String {
        "# Gates: demo\n\nOWNS: src/**\n\n- [ ] G1: fixture exists\n  CHECK: test -f fixtures/ok.txt && echo ok\n  EXPECT: ok\n  EVIDENCE: pending\n\n- [ ] G2: manual review\n  EVIDENCE: pending\n".to_string()
    }

    #[test]
    fn parse_recognizes_gates_and_attrs() {
        let l = parse(&sample_ledger());
        assert!(l.errors.is_empty(), "errors: {:?}", l.errors);
        assert_eq!(l.gates.len(), 2);
        let g1 = &l.gates[0];
        assert_eq!(g1.id, "G1");
        assert!(!g1.checked);
        assert_eq!(g1.check.as_deref(), Some("test -f fixtures/ok.txt && echo ok"));
        assert_eq!(g1.expect.as_deref(), Some("ok"));
        assert_eq!(g1.evidence.as_deref(), Some("pending"));
        assert!(l.gates[1].check.is_none());
    }

    #[test]
    fn parse_ignores_fenced_blocks() {
        let text = "# Gates: x\n\n```md\n- [ ] FAKE: not a real gate\n```\n\n- [ ] G1: real\n  CHECK: true\n  EXPECT: ok\n  EVIDENCE: pending\n";
        let l = parse(text);
        assert!(l.errors.is_empty(), "errors: {:?}", l.errors);
        assert_eq!(l.gates.len(), 1);
        assert_eq!(l.gates[0].id, "G1");
    }

    #[test]
    fn parse_rejects_zero_gates_and_duplicates() {
        let l = parse("# Gates: empty\n");
        assert!(l.errors.iter().any(|e| e.contains("zero gates")));
        let dup = "# Gates: d\n- [ ] G1: a\n  CHECK: true\n  EXPECT: ok\n  EVIDENCE: pending\n- [x] G1: b\n  CHECK: true\n  EXPECT: ok\n  EVIDENCE: pending\n";
        let l = parse(dup);
        assert!(l.errors.iter().any(|e| e.contains("duplicate gate id")));
    }

    #[test]
    fn parse_rejects_runnable_gate_missing_expect() {
        let text = "# Gates: x\n- [ ] G1: a\n  CHECK: true\n  EVIDENCE: pending\n";
        let l = parse(text);
        assert!(l.errors.iter().any(|e| e.contains("CHECK but no EXPECT")));
    }

    #[test]
    fn expect_matches_substring_and_regex() {
        assert!(expect_matches("ok", "everything ok here"));
        assert!(!expect_matches("FAIL", "everything ok here"));
        assert!(expect_matches("/^3\\/3 tiers ok$/", "3/3 tiers ok"));
        assert!(expect_matches("/VERIFIED/i", "all VERIFIED now"));
        assert!(!expect_matches("/VERIFIED/i", "everything passed"));
        // Slash-wrapped: inner slashes literal, dots match any char (incl /).
        assert!(expect_matches("/etc/app/conf/", "to etc/app/conf now"));
        assert!(expect_matches("/v1.2/", "version v1X2 ok"));
        assert!(expect_matches("/v1.2/", "version v1/2 ok"));
        assert!(!expect_matches("/v1.2/", "version v1"));
    }

    #[tokio::test]
    async fn run_executes_checks_and_updates_ledger() {
        let dir = std::env::temp_dir().join(format!("byteai-gates-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fixtures")).unwrap();
        std::fs::write(dir.join("fixtures").join("ok.txt"), "x").unwrap();
        let ledger_path = dir.join("GATES.md");
        std::fs::write(&ledger_path, sample_ledger()).unwrap();
        let p = ledger_path.to_str().unwrap().to_string();

        // status: G1 unmet (pending evidence), G2 manual unmet.
        let st = run_action("status", &p, None).await;
        assert!(st.output.contains("unmet"), "status: {}", st.output);
        assert!(!st.output.contains("1 met"), "status: {}", st.output);

        // run: G1's CHECK passes (file exists) and EXPECT matches -> met.
        let r = run_action("run", &p, None).await;
        assert!(r.output.contains("✓ G1"), "run: {}", r.output);
        let after = std::fs::read_to_string(&ledger_path).unwrap();
        assert!(after.contains("- [x] G1"), "ledger not flipped:\n{after}");
        assert!(after.contains("EVIDENCE: exit 0"), "evidence not recorded:\n{after}");

        // reverify re-runs met gates; G2 stays manual-unmet.
        let rv = run_action("reverify", &p, None).await;
        assert!(rv.output.contains("1 met, 1 unmet"), "reverify: {}", rv.output);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_fails_when_expect_not_matched() {
        let dir = std::env::temp_dir().join(format!("byteai-gates-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ledger_path = dir.join("GATES.md");
        let text = "# Gates: fail\n- [ ] G1: must fail\n  CHECK: echo nope\n  EXPECT: WANTED\n  EVIDENCE: pending\n";
        std::fs::write(&ledger_path, text).unwrap();
        let p = ledger_path.to_str().unwrap().to_string();
        let r = run_action("run", &p, None).await;
        assert!(r.output.contains("✗ G1"), "run: {}", r.output);
        let after = std::fs::read_to_string(&ledger_path).unwrap();
        assert!(after.contains("- [ ] G1"), "box must stay open:\n{after}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
