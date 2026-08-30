//! Smoke tests for built-in tools (verify, memory, skills, review, spawn).
//! These verify the tool definitions parse and the tool registry works.

use byteai_tools::{Registry, ToolContext};

#[test]
fn tool_defs_are_valid() {
    let ctx = ToolContext::new(std::env::temp_dir().join("byteai_test_registry"));
    let reg = Registry::builtins(&ctx);
    let tools = reg.names();
    assert!(!tools.is_empty(), "registry should have built-in tools");
    for name in &tools {
        let t = reg.get(name).expect("registered tool must be gettable");
        assert!(!t.def().name.is_empty(), "tool name must not be empty");
        assert!(!t.def().description.is_empty(), "tool {} description must not be empty", t.def().name);
        let params = &t.def().parameters;
        assert!(params.is_object(), "tool {} parameters must be an object", t.def().name);
        assert!(params.get("type").and_then(|v| v.as_str()) == Some("object"), "tool {} parameters must type=object", t.def().name);
    }
}

#[test]
fn verify_tool_parses_known_project() {
    let reg = Registry::builtins(&ToolContext::new(std::env::temp_dir()));
    let tool = reg.get("verify").expect("verify tool should be registered");
    let def = tool.def();
    assert!(def.description.contains("Verification gate"), "bad verify desc: {}", def.description);
}

#[test]
fn memory_tool_parses_actions() {
    let reg = Registry::builtins(&ToolContext::new(std::env::temp_dir()));
    let tool = reg.get("memory").expect("memory tool should be registered");
    let def = tool.def();
    assert!(def.description.contains("Durable memory"), "bad memory desc: {}", def.description);
}

#[test]
fn skills_tool_parses_actions() {
    let reg = Registry::builtins(&ToolContext::new(std::env::temp_dir()));
    let tool = reg.get("skills").expect("skills tool should be registered");
    let def = tool.def();
    assert!(def.description.contains("Skill system"), "bad skills desc: {}", def.description);
}

#[test]
fn review_tool_smoke() {
    let reg = Registry::builtins(&ToolContext::new(std::env::temp_dir()));
    let tool = reg.get("review").expect("review tool should be registered");
    let def = tool.def();
    assert!(def.description.contains("Independent review"), "bad review desc: {}", def.description);
}

#[test]
fn new_tools_registered() {
    let reg = Registry::builtins(&ToolContext::new(std::env::temp_dir()));
    for name in ["fetch", "graph", "plan", "plugin"] {
        let tool = reg.get(name).unwrap_or_else(|| panic!("{name} tool should be registered"));
        assert!(!tool.def().name.is_empty());
    }
}