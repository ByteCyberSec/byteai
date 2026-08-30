//! `schedule` — durable background/scheduled tasks (deer-flow + Hermes
//! cronjob parity).
//!
//! Create, inspect, pause, resume, trigger, delete durable background jobs
//! persisted to disk. Each job stores a prompt; a background worker (see
//! `byteai` TUI/REPL) runs due jobs and records results.
//!
//! Scheduling modes:
//! - `interval_s`: repeat every N seconds (0 = one-shot).
//! - `cron`: standard 5-field cron expression (e.g. `0 9 * * 1-5`).
//! - `script`: path to an executable script — run it directly (no_agent
//!   mode, Hermes script-mode parity). stdout is captured as the result.
//!
//! Prompt execution: if the prompt starts with `!` or `cmd:`, it runs as a
//! shell command; otherwise it's recorded as "queued" for the LLM worker.

use std::path::PathBuf;
use std::sync::Mutex;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};
use crate::cron::CronExpr;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub prompt: String,
    /// Repeat interval in seconds; 0 = one-shot.
    pub interval_s: u64,
    /// Optional standard cron expression; when set, overrides interval_s.
    pub cron: String,
    /// Optional script path to execute directly (no_agent mode).
    pub script: String,
    /// Unix epoch ms of next run.
    pub next_run_ms: u64,
    pub paused: bool,
    pub runs: u64,
    pub last_result: String,
    pub last_run_ms: u64,
}

impl Job {
    /// Compute the next run timestamp after the given epoch-ms from this job's
    /// schedule. For cron jobs this uses the CronExpr parser; otherwise the
    /// interval (or one-shot semantics) applies.
    pub fn compute_next_run_ms(&self, after_ms: u64) -> u64 {
        if !self.cron.is_empty() {
            // cron is in seconds; `after` is ms. Next strictly after now.
            if let Ok(expr) = CronExpr::parse(&self.cron) {
                let after_s = (after_ms / 1000).saturating_sub(1);
                if let Some(next) = expr.next_after(after_s) {
                    return next * 1000;
                }
            }
            // fall back to +1 day if the cron can't schedule (shouldn't happen)
            return after_ms + 86_400_000;
        }
        if self.interval_s == 0 {
            // one-shot: schedule "never" after first run (paused flag marks done)
            return u64::MAX;
        }
        after_ms + self.interval_s * 1000
    }
}

pub struct ScheduleTool {
    path: Mutex<PathBuf>,
}

impl ScheduleTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { path: Mutex::new(data_dir.join("schedule.json")) }
    }

    pub fn load(&self) -> Vec<Job> {
        let p = self.path.lock().unwrap();
        std::fs::read_to_string(&*p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, jobs: &[Job]) {
        let p = self.path.lock().unwrap();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&*p, serde_json::to_string_pretty(jobs).unwrap_or_default());
    }

    /// Run a due job's script/command with a hard wall-clock bound.
    ///
    /// The old implementation called `Command::output()` synchronously, which
    /// BLOCKS forever on a hung or long-running job (e.g. a 10-minute backup
    /// script) — and `tick()` runs at the start of every REPL iteration and
    /// one-shot turn, so the user saw byteai "frozen" for the script's whole
    /// duration. This runner spawns the process, drains stdout/stderr on
    /// background threads (so a chatty script can't deadlock on a full pipe
    /// buffer), and kills the child if it exceeds `max_secs`.
    fn run_job_bounded(script: &str, max_secs: u64) -> String {
        use std::io::Read;
        let mut child = match std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return format!("error: {e:#}"),
        };
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let out_t = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = stdout.take() {
                let _ = r.read_to_string(&mut s);
            }
            s
        });
        let err_t = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = stderr.take() {
                let _ = r.read_to_string(&mut s);
            }
            s
        });
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if started.elapsed().as_secs() >= max_secs {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = out_t.join();
                        let _ = err_t.join();
                        return format!("error: job exceeded {max_secs}s timeout and was killed");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("error: {e:#}");
                }
            }
        }
        let mut text = out_t.join().unwrap_or_default().trim().to_string();
        let err = err_t.join().unwrap_or_default().trim().to_string();
        if !err.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("[stderr] {err}"));
        }
        text.truncate(1000);
        text
    }

    /// Run any due (non-paused) jobs; returns the jobs that fired. Called by
    /// the TUI/REPL background worker. Each fired job executes its script or
    /// shell command (or records "queued" for the LLM worker) and logs its
    /// outcome, then reschedules using the job's schedule mode.
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
        // Wall-clock cap per job so a hung script can't freeze the REPL/turn.
        const JOB_MAX_SECS: u64 = 30;
        for i in due.into_iter().rev() {
            let mut j = jobs[i].clone();
            j.runs += 1;
            j.last_run_ms = now_ms;
            // Execute the job: script mode (no_agent) wins, then cmd-style
            // prompt, then "queued" for the LLM worker.
            if !j.script.is_empty() {
                let p = j.script.clone();
                j.last_result = Self::run_job_bounded(&p, JOB_MAX_SECS);
            } else {
                let looks_cmd = j.prompt.starts_with('!') || j.prompt.starts_with("cmd:");
                if looks_cmd {
                    let cmd = j.prompt.trim_start_matches('!').trim_start_matches("cmd:").trim().to_string();
                    j.last_result = Self::run_job_bounded(&cmd, JOB_MAX_SECS);
                } else {
                    j.last_result = format!("queued (worker will run prompt, {})", j.prompt.chars().take(60).collect::<String>());
                }
            }
            // Reschedule.
            j.next_run_ms = if j.interval_s == 0 && j.cron.is_empty() {
                u64::MAX // one-shot: done
            } else {
                j.compute_next_run_ms(now_ms)
            };
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
            description: "Durable background jobs. Actions: create {name,prompt?,interval_s?,cron?,script?}, \
list, pause {id}, resume {id}, trigger {id}, delete {id}. \
Scheduling: interval_s (seconds, 0=one-shot) OR cron (standard 5-field expression like '0 9 * * 1-5'). \
Script mode: pass `script` (path to an executable) to run it directly without an LLM (no_agent). \
Prompt mode: prompt starting with '!' or 'cmd:' runs as a shell command; otherwise the background worker runs it as an LLM task. \
Jobs persist and run in the background worker."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create","list","pause","resume","trigger","delete"]},
                    "name": {"type": "string"},
                    "prompt": {"type": "string", "description": "Job prompt; prefix '!' or 'cmd:' to run as a shell command"},
                    "interval_s": {"type": "integer", "description": "Repeat interval in seconds (0 = one-shot)"},
                    "cron": {"type": "string", "description": "Standard 5-field cron expression (overrides interval_s)"},
                    "script": {"type": "string", "description": "Path to an executable script to run directly (no_agent mode)"},
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
        let cron = args.get("cron").and_then(|a| a.as_str()).unwrap_or("").to_string();
        let script = args.get("script").and_then(|a| a.as_str()).unwrap_or("").to_string();
        let id = args.get("id").and_then(|a| a.as_str()).unwrap_or("").to_string();

        let mut out = String::new();
        match action.as_str() {
            "create" => {
                if prompt.is_empty() && script.is_empty() {
                    out.push_str("ERROR: `prompt` or `script` required for create\n");
                } else if !cron.is_empty() && crate::cron::CronExpr::parse(&cron).is_err() {
                    out.push_str(&format!("ERROR: invalid cron expression {cron:?} — use 5 fields (min hour dom mon dow)\n"));
                } else {
                    let mut jobs = self.load();
                    let job_id = format!("job-{}", jobs.len() + 1);
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                    let name_clone = name.clone();
                    let job = Job {
                        id: job_id.clone(),
                        name,
                        prompt,
                        interval_s,
                        cron,
                        script,
                        next_run_ms: now, // due immediately
                        paused: false,
                        runs: 0,
                        last_result: String::new(),
                        last_run_ms: 0,
                    };
                    out.push_str(&format!("Created {job_id} ({name_clone}) interval={interval_s}s cron={} script={}\n", job.cron, job.script));
                    jobs.push(job);
                    self.save(&jobs);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_schedule(tag: &str) -> (std::path::PathBuf, ScheduleTool) {
        let d = std::env::temp_dir().join(format!("byteai_sched_test_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (d.clone(), ScheduleTool::new(d))
    }

    #[tokio::test]
    async fn create_interval_job_and_tick() {
        let (_d, tool) = tmp_schedule("interval");
        let args = json!({"action":"create","name":"t","prompt":"cmd:echo hi","interval_s":60});
        let out = tool.execute(args).await.output;
        assert!(out.contains("Created job-1"));
        // Immediately due -> tick runs it once.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let fired = tool.tick(now + 1000);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].last_result.contains("hi"), "cmd output: {}", fired[0].last_result);
        // Rescheduled to now+60s.
        assert!(fired[0].next_run_ms > now + 50_000);
    }

    #[tokio::test]
    async fn cron_job_computes_next_run() {
        let (_d, tool) = tmp_schedule("cron");
        let args = json!({"action":"create","name":"daily9","prompt":"cmd:date","cron":"0 9 * * *"});
        let out = tool.execute(args).await.output;
        assert!(out.contains("Created job-1"));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let fired = tool.tick(now + 1000);
        assert_eq!(fired.len(), 1);
        // Next run must be strictly in the future, scheduled by cron.
        assert!(fired[0].next_run_ms > now);
    }

    #[tokio::test]
    async fn invalid_cron_rejected() {
        let (_d, tool) = tmp_schedule("badcron");
        let args = json!({"action":"create","name":"x","prompt":"cmd:date","cron":"60 * * * *"});
        let out = tool.execute(args).await.output;
        assert!(out.contains("invalid cron"));
        assert!(tool.load().is_empty(), "no job saved on invalid cron");
    }

    #[tokio::test]
    async fn script_mode_runs_no_agent() {
        let (_d, tool) = tmp_schedule("script");
        let script_path = std::env::temp_dir()
            .join(format!("byteai_sched_script_{}.sh", std::process::id()));
        std::fs::write(&script_path, "#!/bin/sh\necho script-output-ok\n").unwrap();
        std::fs::set_permissions(&script_path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let args = json!({"action":"create","name":"s","script": script_path.to_string_lossy(),"interval_s":0});
        let out = tool.execute(args).await.output;
        assert!(out.contains("Created job-1"));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let fired = tool.tick(now + 1000);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].last_result.contains("script-output-ok"), "script output: {}", fired[0].last_result);
        let _ = std::fs::remove_file(&script_path);
    }

    #[tokio::test]
    async fn pause_and_delete() {
        let (_d, tool) = tmp_schedule("pause");
        let args = json!({"action":"create","name":"p","prompt":"cmd:echo x","interval_s":10});
        tool.execute(args).await.output;
        let jobs = tool.load();
        assert_eq!(jobs.len(), 1);
        let id = jobs[0].id.clone();
        let out = tool.execute(json!({"action":"pause","id":id})).await.output;
        assert!(out.contains("Paused"));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        assert!(tool.tick(now + 1000).is_empty(), "paused job must not fire");
        let out = tool.execute(json!({"action":"delete","id":id})).await.output;
        assert!(out.contains("Deleted"));
        assert!(tool.load().is_empty());
    }

    #[test]
    fn run_job_bounded_kills_hung_script() {
        // A script that never exits must be killed at the bound, NOT hang
        // the REPL/turn start forever (the old Command::output() behavior).
        let out = ScheduleTool::run_job_bounded("sleep 300", 1);
        assert!(out.contains("timeout") && out.contains("killed"), "hung script must be killed: {out}");
    }

    #[test]
    fn run_job_bounded_captures_fast_output() {
        let out = ScheduleTool::run_job_bounded("echo bounded-ok", 5);
        assert!(out.contains("bounded-ok"), "fast job output must be captured: {out}");
    }
}
