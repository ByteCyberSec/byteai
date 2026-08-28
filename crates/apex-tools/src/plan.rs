//! Plan tool (planning-with-files pattern). Persistent markdown plan files in
//! <data_dir>/plans/. Crash-proof: the plan is a plain .md on disk, re-injected
//! on every turn. Deterministic completion gate: a plan is COMPLETE only when
//! every item is checked.
//!
//! Actions:
//!   new <title> <items...>  — create a plan with checklist items
//!   list                    — show all plans
//!   show <name>             — show one plan with status
//!   check <name> <item>     — mark an item done (matched by substring)
//!   add <name> <item>       — append an item
//!   status <name>           — items done/total + COMPLETE gate
//!   delete <name>           — remove a plan

use std::path::{Path, PathBuf};

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

pub struct PlanTool {
    plans_dir: PathBuf,
}

impl PlanTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("plans");
        let _ = std::fs::create_dir_all(&dir);
        Self { plans_dir: dir }
    }
}

impl Default for PlanTool {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .to_lowercase()
}

fn plan_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.md", safe_name(name)))
}

fn parse_plan(text: &str) -> (String, Vec<(String, bool)>) {
    let mut title = String::new();
    let mut items = Vec::new();
    for line in text.lines() {
        if line.starts_with("# ") && title.is_empty() {
            title = line[2..].trim().to_string();
        } else if let Some(rest) = line.strip_prefix("- [ ] ") {
            items.push((rest.trim().to_string(), false));
        } else if let Some(rest) = line.strip_prefix("- [x] ") {
            items.push((rest.trim().to_string(), true));
        }
    }
    (title, items)
}

impl Tool for PlanTool {
    fn name(&self) -> &'static str {
        "plan"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "plan".into(),
            description: "Persistent file-based planning (planning-with-files pattern). \
Plans are markdown checklists in <data_dir>/plans/ — crash-proof, re-readable across turns. \
Actions: new <title> <items>, list, show <name>, check <name> <item>, add <name> <item>, \
status <name>, delete <name>. A plan is COMPLETE only when every item is checked. \
Use for multi-step tasks instead of trusting the conversation window.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["new", "list", "show", "check", "add", "status", "delete"] },
                    "name": { "type": "string", "description": "Plan name (title for new)" },
                    "items": { "type": "array", "items": { "type": "string" }, "description": "Checklist items for new" },
                    "item": { "type": "string", "description": "Item text for check/add (check matches by substring)" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let dir = self.plans_dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();

            match action.as_str() {
                "new" => {
                    if name.is_empty() {
                        return ok_outcome("", "plan", "ERROR: `name` (plan title) required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let items: Vec<String> = args.get("items").and_then(|i| i.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    if items.is_empty() {
                        return ok_outcome("", "plan", "ERROR: `items` (checklist) required for new".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let path = plan_path(&dir, &name);
                    if path.exists() {
                        return ok_outcome("", "plan", format!("plan {:?} already exists — use add/check", name), started.elapsed().as_millis() as u64);
                    }
                    let mut md = format!("# {name}\n\n");
                    for it in &items {
                        md.push_str(&format!("- [ ] {it}\n"));
                    }
                    match std::fs::write(&path, md) {
                        Ok(_) => ok_outcome("", "plan", format!("created plan {:?} with {} item(s) at {}", name, items.len(), path.display()), started.elapsed().as_millis() as u64),
                        Err(e) => ok_outcome("", "plan", format!("write failed: {e}"), started.elapsed().as_millis() as u64),
                    }
                }
                "list" => {
                    let mut out = String::new();
                    let mut plans = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        for e in entries.flatten() {
                            let p = e.path();
                            if p.extension().and_then(|x| x.to_str()) == Some("md")
                                && let Ok(text) = std::fs::read_to_string(&p) {
                                    let (title, items) = parse_plan(&text);
                                    let done = items.iter().filter(|(_, d)| *d).count();
                                    plans.push((title, done, items.len(), p));
                                }
                        }
                    }
                    if plans.is_empty() {
                        return ok_outcome("", "plan", "no plans yet — use `new` to create one".to_string(), started.elapsed().as_millis() as u64);
                    }
                    for (title, done, total, p) in plans {
                        let status = if total > 0 && done == total { "COMPLETE" } else { "in progress" };
                        out.push_str(&format!("  {:<32} {done}/{total} {status}  ({})\n", title, p.file_name().unwrap_or_default().to_string_lossy()));
                    }
                    ok_outcome("", "plan", out, started.elapsed().as_millis() as u64)
                }
                "show" | "status" => {
                    if name.is_empty() {
                        return ok_outcome("", "plan", "ERROR: `name` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let path = plan_path(&dir, &name);
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        return ok_outcome("", "plan", format!("plan {name:?} not found"), started.elapsed().as_millis() as u64);
                    };
                    let (title, items) = parse_plan(&text);
                    let done = items.iter().filter(|(_, d)| *d).count();
                    let total = items.len();
                    let complete = total > 0 && done == total;
                    let mut out = String::new();
                    out.push_str(&format!("# {title}  ({done}/{total} {})\n", if complete { "COMPLETE" } else { "in progress" }));
                    for (i, (item, d)) in items.iter().enumerate() {
                        out.push_str(&format!("  {} {}\n", if *d { "[x]" } else { "[ ]" }, item));
                        if i >= 40 && total > 41 {
                            out.push_str(&format!("  ... {} more\n", total - 41));
                            break;
                        }
                    }
                    if action == "status" {
                        out.push_str(&format!("GATE: {}", if complete { "COMPLETE — all items done" } else { "INCOMPLETE — remaining items must be checked" }));
                    }
                    ok_outcome("", "plan", out, started.elapsed().as_millis() as u64)
                }
                "check" => {
                    if name.is_empty() || args.get("item").and_then(|i| i.as_str()).unwrap_or("").is_empty() {
                        return ok_outcome("", "plan", "ERROR: `name` and `item` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let item = args.get("item").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let path = plan_path(&dir, &name);
                    let Ok(text) = std::fs::read_to_string(&path) else {
                        return ok_outcome("", "plan", format!("plan {name:?} not found"), started.elapsed().as_millis() as u64);
                    };
                    let mut updated = String::new();
                    let mut matched = false;
                    for line in text.lines() {
                        if !matched && line.starts_with("- [ ] ") && line[6..].contains(&item) {
                            updated.push_str(&format!("- [x] {}\n", &line[6..]));
                            matched = true;
                        } else {
                            updated.push_str(line);
                            updated.push('\n');
                        }
                    }
                    if !matched {
                        return ok_outcome("", "plan", format!("no unchecked item contains {item:?}"), started.elapsed().as_millis() as u64);
                    }
                    match std::fs::write(&path, updated) {
                        Ok(_) => {
                            let (_, items) = parse_plan(&std::fs::read_to_string(&path).unwrap_or_default());
                            let done = items.iter().filter(|(_, d)| *d).count();
                            let complete = !items.is_empty() && done == items.len();
                            ok_outcome("", "plan", format!("checked item {item:?} ({done}/{} {})", items.len(), if complete { "COMPLETE" } else { "in progress" }), started.elapsed().as_millis() as u64)
                        }
                        Err(e) => ok_outcome("", "plan", format!("write failed: {e}"), started.elapsed().as_millis() as u64),
                    }
                }
                "add" => {
                    if name.is_empty() || args.get("item").and_then(|i| i.as_str()).unwrap_or("").is_empty() {
                        return ok_outcome("", "plan", "ERROR: `name` and `item` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let item = args.get("item").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let path = plan_path(&dir, &name);
                    let Ok(mut text) = std::fs::read_to_string(&path) else {
                        return ok_outcome("", "plan", format!("plan {name:?} not found"), started.elapsed().as_millis() as u64);
                    };
                    text.push_str(&format!("- [ ] {item}\n"));
                    match std::fs::write(&path, text) {
                        Ok(_) => ok_outcome("", "plan", format!("added item {item:?} to plan {name:?}"), started.elapsed().as_millis() as u64),
                        Err(e) => ok_outcome("", "plan", format!("write failed: {e}"), started.elapsed().as_millis() as u64),
                    }
                }
                "delete" => {
                    if name.is_empty() {
                        return ok_outcome("", "plan", "ERROR: `name` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let path = plan_path(&dir, &name);
                    match std::fs::remove_file(&path) {
                        Ok(_) => ok_outcome("", "plan", format!("deleted plan {name:?}"), started.elapsed().as_millis() as u64),
                        Err(e) => ok_outcome("", "plan", format!("delete failed: {e}"), started.elapsed().as_millis() as u64),
                    }
                }
                other => ok_outcome("", "plan", format!("ERROR: unknown action {other:?}"), started.elapsed().as_millis() as u64),
            }
        })
    }
}
