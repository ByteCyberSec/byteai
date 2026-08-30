//! Smart Tool Selection — "unlimited tools without context rot."
//!
//! Adapted from isair/jarvis's tool-selection system (the feature that lets a
//! local voice agent expose unlimited MCPs without slowing the model down),
//! integrated in the spirit of OpenJarvis's canonical ROUTE step.
//!
//! ByteAi has 40+ tools. Sending every tool def to the model on every call
//! wastes tokens, and small models drift under the prompt pressure (they
//! forget their tools' semantics or echo the whole catalogue). Instead, we
//! score the tool registry against the current task and expose only the
//! relevant subset — always keeping the core harness so the agent can never
//! be left unable to read/edit/search. When the model calls a tool outside
//! the selection, the caller grows the set on demand.

use apex_types::ToolDef;
use std::collections::HashSet;

/// Selection strategy (OpenJarvis-style enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSelectStrategy {
    /// Expose every tool (legacy behavior).
    All,
    /// Keyword-score tools against the task and expose the relevant subset.
    Auto,
}

/// Tools that must ALWAYS be available, regardless of the task — the core
/// harness. Dropping these would leave the agent unable to do its job.
pub const ALWAYS_KEEP: &[&str] = &[
    "read", "search", "edit", "shell", "todo", "note", "memory",
    "websearch", "fetch", "skills", "plan", "spawn", "review",
];

/// Minimum total tools to expose (never leave the model stranded). The core
/// `ALWAYS_KEEP` harness already guarantees this in practice; this is the
/// registry-size threshold below which filtering is pointless.
pub const MIN_TOTAL: usize = 8;
/// Default cap on tools exposed per turn (default `tool_select_max`). The
/// always-keep harness (~13) + a handful of task-relevant tools.
pub const DEFAULT_MAX: usize = 16;

/// Common English stop-words excluded from keyword scoring.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "shall",
    "should", "may", "might", "must", "can", "could", "i", "me", "my",
    "you", "your", "he", "she", "it", "we", "they", "them", "this", "that",
    "what", "which", "who", "when", "where", "how", "not", "no", "so", "if",
    "or", "and", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "as", "into", "about", "up", "out", "off", "over", "just",
    "also", "very", "too", "some", "any", "all", "need", "using", "use",
];

/// Tokenize text: lowercase alphanumeric runs, drop stop-words.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1 && !STOP_WORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

/// Score one tool def against the task token set: count overlapping tokens
/// between the task and (name + description), name matches weigh double.
fn score(def: &ToolDef, task_tokens: &HashSet<&str>) -> usize {
    let mut score = 0usize;
    let name_tokens = tokens(&def.name);
    let desc_tokens = tokens(&def.description);
    for t in task_tokens {
        if name_tokens.iter().any(|n| n == t) {
            score += 2;
        } else if desc_tokens.iter().any(|d| d == t) {
            score += 1;
        }
    }
    score
}

/// Select the tool subset to expose for a task.
///
/// - Always keeps the core harness (`ALWAYS_KEEP`).
/// - Scores the rest by keyword overlap with the task, taking the top matches
///   up to `max`.
/// - Small registries (≤ `max` tools) are returned whole — no point filtering.
/// - Returns `All`-behavior when `strategy == All` or the task is empty (can't
///   judge relevance without a task).
pub fn select_tools(
    defs: &[ToolDef],
    task: &str,
    strategy: ToolSelectStrategy,
    max: usize,
) -> Vec<ToolDef> {
    match strategy {
        ToolSelectStrategy::All => defs.to_vec(),
        ToolSelectStrategy::Auto => {
            let task_tokens_vec = tokens(task);
            let task_tokens: HashSet<&str> = task_tokens_vec.iter().map(|s| s.as_str()).collect();
            if task_tokens.is_empty() {
                return defs.to_vec();
            }
            // Small registry: filtering buys nothing.
            if defs.len() <= max {
                return defs.to_vec();
            }
            let mut kept: Vec<ToolDef> = Vec::new();
            let mut scored: Vec<(usize, &ToolDef)> = Vec::new();
            for def in defs {
                if ALWAYS_KEEP.contains(&def.name.as_str()) {
                    kept.push(def.clone());
                } else {
                    let s = score(def, &task_tokens);
                    if s > 0 {
                        scored.push((s, def));
                    }
                }
            }
            // Best matches first, stable for ties.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
            let mut room = max.saturating_sub(kept.len());
            for (_, def) in scored {
                if room == 0 {
                    break;
                }
                // De-duplicate (a tool shouldn't be both always-keep and scored).
                if kept.iter().any(|k| k.name == def.name) {
                    continue;
                }
                kept.push(def.clone());
                room -= 1;
            }
            kept
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(name: &str, desc: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: desc.into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn registry() -> Vec<ToolDef> {
        vec![
            def("read", "read files from disk"),
            def("edit", "edit files"),
            def("shell", "run shell commands"),
            def("search", "search the codebase"),
            def("todo", "manage a todo list"),
            def("note", "write durable notes"),
            def("memory", "search long-term memory"),
            def("websearch", "search the web"),
            def("fetch", "fetch a web page"),
            def("skills", "load and create skills"),
            def("plan", "write an implementation plan"),
            def("spawn", "spawn parallel subagents"),
            def("review", "review code changes"),
            def("github", "create pull requests and issues on github"),
            def("notify", "send a webhook notification to slack"),
            def("weather", "get weather for a city"),
            def("git", "git operations: status, commit, diff"),
            def("kanban", "manage a kanban task board"),
            def("council", "multi-model deliberation vote"),
            def("moa", "mixture of agents fusion"),
        ]
    }

    #[test]
    fn all_strategy_returns_everything() {
        let defs = registry();
        let out = select_tools(&defs, "anything", ToolSelectStrategy::All, 5);
        assert_eq!(out.len(), defs.len());
    }

    #[test]
    fn auto_keeps_core_and_relevant() {
        let defs = registry(); // 20 tools > max 16 → filtering active
        // GitHub task: github must be selected; weather must NOT be.
        let out = select_tools(&defs, "open a pull request on github", ToolSelectStrategy::Auto, 16);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"github"), "github should be selected: {names:?}");
        assert!(!names.contains(&"weather"), "weather should be dropped: {names:?}");
        assert!(!names.contains(&"kanban"), "kanban should be dropped: {names:?}");
        // Core harness always present.
        for core in ["read", "search", "edit", "shell", "skills", "plan", "spawn"] {
            assert!(names.contains(&core), "core {core} missing: {names:?}");
        }
    }

    #[test]
    fn auto_caps_at_max() {
        let defs = registry();
        // Cap never exceeds max and never drops the core harness.
        let out = select_tools(&defs, "open a pull request on github", ToolSelectStrategy::Auto, 14);
        assert!(out.len() <= 14, "cap violated: {}", out.len());
        for core in ["read", "edit", "shell", "search"] {
            assert!(out.iter().any(|d| d.name == *core), "core {core} missing");
        }
        // Huge cap returns everything.
        let out2 = select_tools(&defs, "open a pull request on github", ToolSelectStrategy::Auto, 100);
        assert_eq!(out2.len(), defs.len());
    }

    #[test]
    fn auto_with_empty_task_returns_everything() {
        let defs = registry();
        let out = select_tools(&defs, "   ", ToolSelectStrategy::Auto, 8);
        assert_eq!(out.len(), defs.len(), "cannot judge relevance without a task");
    }

    #[test]
    fn auto_never_drops_always_keep() {
        let defs = registry();
        // A task about something completely unrelated to core tools.
        let out = select_tools(&defs, "check the weather in paris", ToolSelectStrategy::Auto, 16);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"weather"), "weather should be selected");
        for core in ALWAYS_KEEP {
            if defs.iter().any(|d| d.name == *core) {
                assert!(names.contains(core), "core {core} must survive: {names:?}");
            }
        }
    }

    #[test]
    fn auto_finds_relevant_tool_by_description() {
        let defs = registry();
        // "notify" is not in the task verbatim, but its description matches.
        let out = select_tools(&defs, "send a slack notification", ToolSelectStrategy::Auto, 16);
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"notify"), "notify should match via description: {names:?}");
    }
}
