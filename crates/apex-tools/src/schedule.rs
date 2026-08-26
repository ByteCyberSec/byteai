//! `schedule` — durable background/scheduled tasks (deer-flow pattern).
//! Top harness feature: create, inspect, pause, resume, trigger, delete
//! durable background jobs persisted to disk. Jobs store a prompt; a
//! background worker (see `byteai` TUI/REPL) runs due jobs and records
//! results.

use std::path::PathBuf;
use std::sync::Mutex;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub prompt: String,
    /// cron-ish interval seconds; 0 = one-shot (runs once at `next_run`).
    pub interval_s: u64,
    /// Unix epoch ms of next run.
    pub next_run_ms: u64,
    pub paused: bool,
    pub runs: u64,
    pub last_result: String,
    pub last_run_ms: u64,
}

pub struct ScheduleTool {
    path: Mutex<PathBuf>,
}

impl ScheduleTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { path: Mutex::new(data_dir.join("schedule.json")) }
    }

    fn load(&self) -> Vec<Job> {
        let p = self.path.lock().unwrap();
        std::fs::read_to_string(&*p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, jobs: &[Job]) {
        let p = self.path.lock().unwrap();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&*p, serde_json::to_string_pretty(jobs).unwrap_or_default());
    }

    /// Run any due (non-paused) jobs; returns the jobs that fired. Called by
    /// the TUI/REPL background worker. Each fired job logs its outcome.
    pub fn tick(&self, now_ms: u64) -> Vec<Job> {
        let mut jobs = self.load();
        let mut fired = Vec::new();
        let mut due: Vec<usize> = jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| !j.paused && j.next_run_ms <= now_ms)
            .map(|(i, _)| i)
            .collect();
        due.sort();
        for i in due.into_iter().rev() {
            let mut j = jobs[i].clone();
            j.runs += 1;
            j.last_run_ms = now_ms;
            if j.interval_s == 0 {
                j.paused = true; // one-shot: mark done
            } else {
                j.next_run_ms = now_ms + j.interval_s * 1000;
            }
            // Execute the job prompt via a shell if it looks like a command,
            // otherwise record "queued" (LLM execution is done by the worker).
            let looks_cmd = j.prompt.starts_with("!") || j.prompt.starts_with("cmd:");
            if looks_cmd {
                let cmd = j.prompt.trim_start_matches('!').trim_start_matches("cmd:").trim().to_string();
                match std::process::Command::new("sh").arg("-c").arg(&cmd).output() {
                    Ok(o) => j.last_result = String::from_utf8_lossy(&o.stdout).trim().chars().take(500).collect(),
                    Err(e) => j.last_result = format!("error: {e:#}"),
                }
            } else {
                j.last_result = format!("queued (worker will run prompt, {})", j.prompt.chars().take(60).collect::<String>());
            }
            fired.push(j.clone());
            jobs[i] = j;
        }
        self.save(&jobs);
        fired
    }
}

impl Tool for ScheduleTool {
    fn name(&self) -> &'static str {
        "schedule"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "schedule".into(),
            description: "Durable background jobs. Actions: create {name,prompt,interval_s?}, list, pause {id}, resume {id}, trigger {id}, delete {id}. Jobs persist and run in the background worker.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create","list","pause","resume","trigger","delete"]},
                    "name": {"type": "string"},
                    "prompt": {"type": "string", "description": "Job prompt; prefix '!' or 'cmd:' to run as a shell command"},
                    "interval_s": {"type": "integer", "description": "Repeat interval in seconds (0 = one-shot)"},
                    "id": {"type": "string", "description": "Job id"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let started = std::time::Instant::now();
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
        let name = args.get("name").and_then(|a| a.as_str()).unwrap_or("job").to_string();
        let prompt = args.get("prompt").and_then(|a| a.as_str()).unwrap_or("").to_string();
        let interval_s = args.get("interval_s").and_then(|a| a.as_u64()).unwrap_or(0);
        let id = args.get("id").and_then(|a| a.as_str()).unwrap_or("").to_string();

        let mut out = String::new();
        match action.as_str() {
            "create" => {
                if prompt.is_empty() {
                    out.push_str("ERROR: `prompt` required for create\n");
                } else {
                    let mut jobs = self.load();
                    let job_id = format!("job-{}", jobs.len() + 1);
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                    let name_clone = name.clone();
                    jobs.push(Job {
                        id: job_id.clone(),
                        name,
                        prompt,
                        interval_s,
                        next_run_ms: now, // due immediately
                        paused: false,
                        runs: 0,
                        last_result: String::new(),
                        last_run_ms: 0,
                    });
                    self.save(&jobs);
                    out.push_str(&format!("Created {job_id} ({name_clone}) interval={interval_s}s\n"));
                }
            }
            "list" => {
                let jobs = self.load();
                if jobs.is_empty() {
                    out.push_str("No scheduled jobs.\n");
                }
                for j in jobs {
                    out.push_str(&format!(
                        "{} {} paused={} runs={} next={}ms result={}\n",
                        j.id, j.name, j.paused, j.runs, j.next_run_ms,
                        j.last_result.chars().take(60).collect::<String>()
                    ));
                }
            }
            "pause" | "resume" | "trigger" | "delete" => {
                let mut jobs = self.load();
                match jobs.iter_mut().find(|j| j.id == id) {
                    Some(j) => match action.as_str() {
                        "pause" => { j.paused = true; out.push_str(&format!("Paused {id}\n")); }
                        "resume" => { j.paused = false; out.push_str(&format!("Resumed {id}\n")); }
                        "trigger" => { j.next_run_ms = 0; out.push_str(&format!("Triggered {id} (runs on next tick)\n")); }
                        _ => {}
                    },
                    None => out.push_str(&format!("ERROR: no job {id}\n")),
                }
                if action == "delete" {
                    let len = jobs.len();
                    jobs.retain(|j| j.id != id);
                    if jobs.len() < len {
                        out.push_str(&format!("Deleted {id}\n"));
                    }
                }
                self.save(&jobs);
            }
            other => out.push_str(&format!("ERROR: unknown action {other:?}\n")),
        }
        let elapsed = started.elapsed().as_millis() as u64;
        Box::pin(async move { ok_outcome("", self.name(), out, elapsed) })
    }
}