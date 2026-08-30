//! `conductor` — hierarchical task orchestration (unique to ByteAI).
//!
//! A persistent execution graph: goals → phases → tasks, with dependency
//! gating (a task cannot start until its dependencies are done), progress
//! tracking, and outcome synthesis. Deeper than `plan` (flat checklist) and
//! `kanban` (columns): this is a DAG with blocked/ready/in-progress/done
//! states and a final synthesis step.
//!
//! Storage: `<data>/conductor/<name>.json` — survives restarts and resumes.

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Blocked,
    Ready,
    InProgress,
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub deps: Vec<String>,
    pub state: TaskState,
    pub outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conductor {
    pub name: String,
    pub created_ms: u64,
    pub phases: Vec<Phase>,
    pub closed: bool,
}

impl Conductor {
    /// Recompute states from the dependency graph after any mutation.
    fn recompute(&mut self) {
        // Snapshot current done-task set to break the borrow on self.
        let done_names: std::collections::HashSet<String> = self
            .phases
            .iter()
            .flat_map(|p| p.tasks.iter())
            .filter(|t| t.state == TaskState::Done)
            .map(|t| t.name.clone())
            .collect();
        for phase in &mut self.phases {
            for task in &mut phase.tasks {
                if task.state == TaskState::Done {
                    continue;
                }
                let deps_done = task.deps.iter().all(|d| done_names.contains(d.as_str()));
                task.state = if deps_done { TaskState::Ready } else { TaskState::Blocked };
            }
        }
    }

    fn progress(&self) -> (usize, usize) {
        let total: usize = self.phases.iter().map(|p| p.tasks.len()).sum();
        let done: usize = self
            .phases
            .iter()
            .flat_map(|p| p.tasks.iter())
            .filter(|t| t.state == TaskState::Done)
            .count();
        (done, total)
    }

    fn find_task_mut(&mut self, name: &str) -> Option<&mut Task> {
        self.phases
            .iter_mut()
            .flat_map(|p| p.tasks.iter_mut())
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    fn summarize(&self) -> String {
        let mut out = String::new();
        for phase in &self.phases {
            let done = phase.tasks.iter().filter(|t| t.state == TaskState::Done).count();
            out.push_str(&format!("  {} — {}/{} done\n", phase.name, done, phase.tasks.len()));
            for t in &phase.tasks {
                let mark = match t.state {
                    TaskState::Done => "✓",
                    TaskState::InProgress => "▶",
                    TaskState::Ready => "○",
                    TaskState::Blocked => "⊘",
                };
                out.push_str(&format!("    {mark} {}\n", t.name));
                if !t.outcome.is_empty() {
                    out.push_str(&format!("        → {}\n", t.outcome));
                }
            }
        }
        out
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
        .to_lowercase()
}

pub struct ConductorTool {
    dir: PathBuf,
}

impl ConductorTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("conductor");
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", safe_name(name)))
    }

    fn load(&self, name: &str) -> Option<Conductor> {
        std::fs::read_to_string(self.path(name))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn save(&self, c: &Conductor) -> bool {
        std::fs::write(self.path(&c.name), serde_json::to_string_pretty(c).unwrap_or_default()).is_ok()
    }
}

impl Tool for ConductorTool {
    fn name(&self) -> &'static str {
        "conductor"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "conductor".into(),
            description: "Hierarchical task orchestration: goals -> phases -> tasks with dependency \
gating (a task stays blocked until its deps finish), progress %, and outcome synthesis. \
Actions: new <name>, phase <name> <phase>, task <name> <phase> <task> [deps...], \
start <name> <task>, done <name> <task> <outcome>, status <name>, blocked <name>, \
synthesize <name>, list, close <name>. Use for complex multi-step work — deeper than plan."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["new", "phase", "task", "start", "done", "status", "blocked", "synthesize", "list", "close"], "description": "What to do." },
                    "name": { "type": "string", "description": "Conductor name." },
                    "phase": { "type": "string", "description": "Phase name (for phase/task)." },
                    "task": { "type": "string", "description": "Task name (for task)." },
                    "deps": { "type": "array", "items": { "type": "string" }, "description": "Task dependency names (for task)." },
                    "item": { "type": "string", "description": "Task name for start/done." },
                    "outcome": { "type": "string", "description": "Result text for done." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let dir = self.dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
            let name = args.get("name").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let phase = args.get("phase").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let task = args.get("task").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let item = args.get("item").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let outcome = args.get("outcome").and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let deps: Vec<String> = args
                .get("deps")
                .and_then(|d| d.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let elapsed = started.elapsed().as_millis() as u64;

            let t = ConductorTool { dir: dir.clone() };

            match action.as_str() {
                "new" => {
                    if name.is_empty() {
                        return ok_outcome("", "conductor", "usage: conductor new <name>".to_string(), elapsed);
                    }
                    if t.load(&name).is_some() {
                        return ok_outcome("", "conductor", format!("conductor {name:?} already exists — use phase/task to build it out"), elapsed);
                    }
                    let c = Conductor { name: name.clone(), created_ms: now_ms(), phases: vec![], closed: false };
                    if t.save(&c) {
                        ok_outcome("", "conductor", format!("conductor {name:?} created — add phases: conductor phase {name} <phase>"), elapsed)
                    } else {
                        ok_outcome("", "conductor", "could not write conductor".to_string(), elapsed)
                    }
                }
                "phase" => {
                    let mut c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    if phase.is_empty() {
                        return ok_outcome("", "conductor", "usage: conductor phase <name> <phase>".to_string(), elapsed);
                    }
                    if c.phases.iter().any(|p| p.name.eq_ignore_ascii_case(&phase)) {
                        return ok_outcome("", "conductor", format!("phase {phase:?} already exists"), elapsed);
                    }
                    c.phases.push(Phase { name: phase.clone(), tasks: vec![] });
                    c.recompute();
                    if t.save(&c) {
                        ok_outcome("", "conductor", format!("phase {phase:?} added to {name:?} — add tasks: conductor task {name} {phase} <task> [deps...]"), elapsed)
                    } else {
                        ok_outcome("", "conductor", "could not write conductor".to_string(), elapsed)
                    }
                }
                "task" => {
                    let mut c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    if task.is_empty() {
                        return ok_outcome("", "conductor", "usage: conductor task <name> <phase> <task> [deps...]".to_string(), elapsed);
                    }
                    let ph = match c.phases.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&phase)) {
                        Some(p) => p,
                        None => return ok_outcome("", "conductor", format!("no phase {phase:?} in {name:?} — add it first"), elapsed),
                    };
                    if ph.tasks.iter().any(|t2| t2.name.eq_ignore_ascii_case(&task)) {
                        return ok_outcome("", "conductor", format!("task {task:?} already exists"), elapsed);
                    }
                    ph.tasks.push(Task { name: task.clone(), deps, state: TaskState::Blocked, outcome: String::new() });
                    c.recompute();
                    if t.save(&c) {
                        ok_outcome("", "conductor", format!("task {task:?} added (phase {phase:?})"), elapsed)
                    } else {
                        ok_outcome("", "conductor", "could not write conductor".to_string(), elapsed)
                    }
                }
                "start" => {
                    let mut c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    let task = match c.find_task_mut(&item) {
                        Some(t) => t,
                        None => return ok_outcome("", "conductor", format!("no task {item:?} in {name:?}"), elapsed),
                    };
                    if task.state == TaskState::Blocked {
                        let deps = task.deps.clone();
                        return ok_outcome("", "conductor", format!("task {item:?} is blocked — finish deps first: {deps:?}"), elapsed);
                    }
                    task.state = TaskState::InProgress;
                    if t.save(&c) {
                        ok_outcome("", "conductor", format!("▶ {item:?} started"), elapsed)
                    } else {
                        ok_outcome("", "conductor", "could not write conductor".to_string(), elapsed)
                    }
                }
                "done" => {
                    let mut c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    let task = match c.find_task_mut(&item) {
                        Some(t) => t,
                        None => return ok_outcome("", "conductor", format!("no task {item:?} in {name:?}"), elapsed),
                    };
                    task.state = TaskState::Done;
                    if !outcome.is_empty() {
                        task.outcome = outcome.clone();
                    }
                    c.recompute();
                    let (done_n, total) = c.progress();
                    let all = done_n == total && total > 0;
                    let msg = if all {
                        format!("✓ {item:?} done — conductor {name:?} COMPLETE ({done_n}/{total}). synthesize to get the summary.")
                    } else {
                        format!("✓ {item:?} done ({done_n}/{total})")
                    };
                    if t.save(&c) {
                        ok_outcome("", "conductor", msg, elapsed)
                    } else {
                        ok_outcome("", "conductor", "could not write conductor".to_string(), elapsed)
                    }
                }
                "status" => {
                    let c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    let (done_n, total) = c.progress();
                    let pct = if total > 0 { (done_n as f64 / total as f64 * 100.0).round() as u64 } else { 0 };
                    let mut out = format!("{} — {}% complete ({done_n}/{total})\n", c.name, pct);
                    out.push_str(&c.summarize());
                    ok_outcome("", "conductor", out, elapsed)
                }
                "blocked" => {
                    let c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    let blocked: Vec<&Task> = c
                        .phases
                        .iter()
                        .flat_map(|p| p.tasks.iter())
                        .filter(|t| t.state == TaskState::Blocked)
                        .collect();
                    if blocked.is_empty() {
                        return ok_outcome("", "conductor", "no blocked tasks — everything is ready or done".to_string(), elapsed);
                    }
                    let mut out = format!("{} blocked task(s):\n", blocked.len());
                    for t in blocked {
                        out.push_str(&format!("  ⊘ {} (deps: {:?})\n", t.name, t.deps));
                    }
                    ok_outcome("", "conductor", out, elapsed)
                }
                "synthesize" => {
                    let c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    let mut out = format!("📋 Synthesis — {}\n", c.name);
                    for phase in &c.phases {
                        out.push_str(&format!("  {}:\n", phase.name));
                        for t in &phase.tasks {
                            let mark = if t.state == TaskState::Done { "✓" } else { "○" };
                            let outcome = if t.outcome.is_empty() { "(no outcome recorded)" } else { &t.outcome };
                            out.push_str(&format!("    {mark} {} → {}\n", t.name, outcome));
                        }
                    }
                    ok_outcome("", "conductor", out, elapsed)
                }
                "list" => {
                    let mut names: Vec<String> = std::fs::read_dir(&t.dir)
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
                                .filter_map(|e| e.file_name().into_string().ok())
                                .map(|f| f.trim_end_matches(".json").to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    names.sort();
                    if names.is_empty() {
                        return ok_outcome("", "conductor", "no conductors yet — create one: conductor new <name>".to_string(), elapsed);
                    }
                    let mut out = format!("{} conductor(s):\n", names.len());
                    for n in &names {
                        let c = t.load(n);
                        let (done_n, total) = c.as_ref().map(|c| c.progress()).unwrap_or((0, 0));
                        let pct = if total > 0 { (done_n as f64 / total as f64 * 100.0).round() as u64 } else { 0 };
                        out.push_str(&format!("  {n} — {pct}%\n"));
                    }
                    ok_outcome("", "conductor", out, elapsed)
                }
                "close" => {
                    let mut c = match t.load(&name) {
                        Some(c) => c,
                        None => return ok_outcome("", "conductor", format!("no conductor {name:?} — create it first"), elapsed),
                    };
                    c.closed = true;
                    if t.save(&c) {
                        ok_outcome("", "conductor", format!("conductor {name:?} closed — archived"), elapsed)
                    } else {
                        ok_outcome("", "conductor", "could not write conductor".to_string(), elapsed)
                    }
                }
                other => ok_outcome("", "conductor", format!("unknown action {other:?} — see /help conductor"), elapsed),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_cond_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn dependency_gating_blocks_and_unblocks() {
        let d = tmp_dir("deps");
        let t = ConductorTool::new(d.clone());

        t.execute(json!({"action": "new", "name": "build login"})).await;
        t.execute(json!({"action": "phase", "name": "build login", "phase": "backend"})).await;
        t.execute(json!({"action": "task", "name": "build login", "phase": "backend", "task": "schema", "deps": []})).await;
        t.execute(json!({"action": "task", "name": "build login", "phase": "backend", "task": "routes", "deps": ["schema"]})).await;

        // routes must be blocked while schema is not done
        let out = t.execute(json!({"action": "status", "name": "build login"})).await;
        assert!(out.output.contains("⊘ routes"), "routes blocked until schema done: {}", out.output);
        assert!(out.output.contains("○ schema"), "schema ready: {}", out.output);

        // starting routes before schema done must fail
        let out = t.execute(json!({"action": "start", "name": "build login", "item": "routes"})).await;
        assert!(out.output.contains("blocked"), "cannot start blocked task: {}", out.output);

        // finish schema -> routes become ready
        t.execute(json!({"action": "start", "name": "build login", "item": "schema"})).await;
        t.execute(json!({"action": "done", "name": "build login", "item": "schema", "outcome": "users table created"})).await;
        let out = t.execute(json!({"action": "status", "name": "build login"})).await;
        assert!(out.output.contains("○ routes"), "routes ready after dep done: {}", out.output);

        // complete routes -> 100%
        t.execute(json!({"action": "start", "name": "build login", "item": "routes"})).await;
        let out = t.execute(json!({"action": "done", "name": "build login", "item": "routes", "outcome": "auth routes wired"})).await;
        assert!(out.output.contains("COMPLETE"), "all done -> complete: {}", out.output);

        // synthesize shows outcomes
        let out = t.execute(json!({"action": "synthesize", "name": "build login"})).await;
        assert!(out.output.contains("users table created"), "synthesis carries outcomes: {}", out.output);
        assert!(out.output.contains("auth routes wired"));

        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn list_shows_progress() {
        let d = tmp_dir("list");
        let t = ConductorTool::new(d.clone());
        t.execute(json!({"action": "new", "name": "write docs"})).await;
        t.execute(json!({"action": "phase", "name": "write docs", "phase": "phase1"})).await;
        t.execute(json!({"action": "task", "name": "write docs", "phase": "phase1", "task": "readme"})).await;
        t.execute(json!({"action": "start", "name": "write docs", "item": "readme"})).await;
        t.execute(json!({"action": "done", "name": "write docs", "item": "readme", "outcome": "docs written"})).await;
        let out = t.execute(json!({"action": "list"})).await;
        assert!(out.output.contains("write-docs — 100%"), "list shows progress: {}", out.output);
        let _ = std::fs::remove_dir_all(&d);
    }
}
