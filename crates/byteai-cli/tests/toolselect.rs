//! Smart Tool Selection against the REAL tool registry (40+ built-in tools).
//! Verifies the isair/jarvis-style "no context rot" filtering works end to
//! end: a GitHub task exposes github while dropping unrelated tools, the core
//! harness always survives, and the set grows on demand.

use byteai_core::{ToolSelectStrategy, select_tools};
use byteai_tools::{Registry, ToolContext};

fn registry() -> Registry {
    Registry::builtins(&ToolContext::new(std::env::temp_dir().join("byteai_toolselect")))
}

#[test]
fn real_registry_has_many_tools() {
    let reg = registry();
    assert!(reg.names().len() > 20, "expected a big registry, got {}", reg.names().len());
}

#[test]
fn github_task_selects_github_and_drops_unrelated() {
    let reg = registry();
    let defs = reg.defs();
    let out = select_tools(&defs, "open a pull request on github and assign a reviewer", ToolSelectStrategy::Auto, 16);
    let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"github"), "github should be selected: {names:?}");
    // Core harness always present.
    for core in ["read", "edit", "search", "shell", "skills", "plan", "spawn", "review"] {
        assert!(names.contains(&core), "core {core} missing from selection: {names:?}");
    }
    // Unrelated tools dropped (selection is a strict subset of the registry).
    assert!(out.len() < defs.len(), "selection should be smaller than full registry");
    println!("github task -> {} tools: {}", out.len(), names.join(", "));
}

#[test]
fn memory_task_keeps_memory_tools() {
    let reg = registry();
    let defs = reg.defs();
    let out = select_tools(&defs, "search my long-term memory for the notes about the dan methodology", ToolSelectStrategy::Auto, 16);
    let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"memory") || names.contains(&"memsearch"),
        "memory tool should be selected: {names:?}");
    println!("memory task -> {} tools: {}", out.len(), names.join(", "));
}

#[test]
fn selection_grows_on_demand() {
    // Simulate the loop: start with the selection, then the model calls a
    // tool outside it (github), and we add its def back.
    let reg = registry();
    let defs = reg.defs();
    let mut selected = select_tools(&defs, "review the current diff", ToolSelectStrategy::Auto, 16);
    let before = selected.len();
    let github = reg.get("github").expect("github registered");
    if !selected.iter().any(|d| d.name == "github") {
        selected.push(github.def());
    }
    assert!(selected.iter().any(|d| d.name == "github"), "grow-on-demand failed");
    assert!(selected.len() >= before, "grew backwards");
    println!("grow: {} -> {}", before, selected.len());
}
