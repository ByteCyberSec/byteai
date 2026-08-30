//! `spawn` — multi-agent orchestration with model routing and Herdr support.
//!
//! Spawns N parallel sub-agents, each a `byteai chat` process with an
//! isolated prompt. Key features:
//!
//! **Model routing**: Each goal is classified (code/reasoning/fast/memory)
//! and the best available model is assigned automatically. If OmniRoute is
//! accessible, it uses capability aliases (auto/best-coding, etc.).
//! Otherwise falls back to the default model.
//!
//! **Herdr support**: When `HERDR_ENV=1` (running inside a Herdr terminal),
//! each sub-agent spawns in its own NEW Herdr TAB — named after a short slug
//! of its task — directly under the CURRENT workspace (no new workspace is
//! ever created). This makes every sub-agent visible, inspectable, and
//! killable in the Herdr TUI while it works.
//!
//! **Dead-provider safety**: every candidate provider is liveness-probed with
//! a tiny chat call before children are routed; broken providers (401 stale
//! key, down endpoint) are skipped so no child dies from a bad provider slot.
//!
//! **Scalability**: Up to 1000 parallel agents (configurable via
//! `max_parallel`). Each child gets its own independent iteration budget
//! (`delegation_max_iterations`, default 250 — Hermes parity).
//!
//! **Lifecycle**: The parent collects all results, then the main agent loop
//! decides whether to assign more tasks based on the magnitude of the job.

use std::sync::Arc;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{BoxFuture, Tool, ToolContext, ok_outcome};

const AGENT_TIMEOUT: u64 = 600; // 10 minutes per child max

/// Kill-list for orphaned children (same as before).
#[derive(Default)]
struct KillList {
    pids: std::sync::Mutex<Vec<u32>>,
}

impl KillList {
    fn register(&self, pid: u32) {
        if pid != 0
            && let Ok(mut p) = self.pids.lock() {
                p.push(pid);
            }
    }
    fn deregister(&self, pid: u32) {
        if let Ok(mut p) = self.pids.lock() {
            p.retain(|&x| x != pid);
        }
    }
    fn kill_all(&self) {
        let pids: Vec<u32> = self.pids.lock().map(|p| p.clone()).unwrap_or_default();
        for pid in pids {
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
        }
    }
}

impl Drop for KillList {
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// Classify a goal string into a model capability hint.
#[allow(dead_code)]
fn classify_goal(goal: &str) -> &'static str {
    let g = goal.to_lowercase();
    let reasoning = ["why", "explain", "prove", "design", "architecture", "review",
                     "analyze", "compare", "trade-off", "optimize", "research"];
    let code = ["implement", "refactor", "fix", "bug", "test", "cargo", "npm",
                "compile", "edit", "write code", "patch", "function", "api", "build"];
    let memory = ["recall", "remember", "what did", "summarize", "search", "notes",
                  "memory", "previous session", "find"];
    let fast = ["yes/no", "reply with exactly", "summarize in one", "what is 2", "hi there", "hello there"];

    let count = |words: &[&str]| words.iter().filter(|w| g.contains(**w)).count();
    let (r, c, m, f) = (count(&reasoning), count(&code), count(&memory), count(&fast));

    if f > 0 && r == 0 && c == 0 { "fast" }
    else if r > c && r > 0 { "reasoning" }
    else if c > 0 { "code" }
    else if m > 0 { "memory" }
    else { "default" }
}

/// Pick the best model for a task class. Uses the default model directly
/// (the auto/best-* routing requires OmniRoute to be the default provider).
#[allow(dead_code)]
fn pick_model(_class: &str, default_model: &str) -> String {
    default_model.to_string()
}

/// A model assignment for one sub-agent: which provider and which model.
#[derive(Debug, Clone)]
struct ModelSlot {
    pub provider: String,
    pub model: String,
}

/// A provider we can probe for liveness (base URL + resolved key + model).
#[derive(Debug, Clone)]
struct ProviderProbe {
    pub name: String,
    pub base_url: String,
    pub key: String,
    pub model: String,
}

/// Model router: reads ALL configured providers from the config file and
/// distributes sub-agents round-robin across them so that NO single
/// provider/model endpoint gets overloaded by a large parallel spawn.
///
/// Why: a cloud provider (e.g. api.b.ai) rate-limits at ~2-3 concurrent
/// requests. If we pin every child to the same model, a 10-agent spawn
/// trips 429s immediately. Instead we interleave:
///   agent 0 -> bai/deepseek-v4-flash
///   agent 1 -> omniroute/deepseek-v4-flash
///   agent 2 -> bai/deepseek-v4-flash
///   ...
/// The local OmniRoute provider (648 models, no rate limit) absorbs most
/// of the parallel load; cloud providers get at most ceil(n/2) children.
///
/// Dead-provider safety: before use, `prune_dead()` fires a real (tiny)
/// chat completion at every candidate provider and DROPS any that fail
/// (401 stale key, endpoint down, model unknown). A broken provider never
/// silently kills the children routed to it. When the provider recovers,
/// it is automatically included again on the next spawn.
struct ModelRouter {
    /// Ordered (provider, model) slots to rotate through.
    slots: Vec<ModelSlot>,
    /// Providers to liveness-probe before the roster is used.
    probes: Vec<ProviderProbe>,
}

impl ModelRouter {
    /// Build the router from the config file at `<data_dir>/config.toml`.
    /// Falls back to a single slot (default provider + model) on any error.
    /// Note: this does NOT probe — call `prune_dead().await` before use.
    fn from_config(data_dir: &std::path::Path, default_model: &str) -> Self {
        let mut slots: Vec<ModelSlot> = Vec::new();
        let mut probes: Vec<ProviderProbe> = Vec::new();

        let cfg_path = data_dir.join("config.toml");
        if let Ok(text) = std::fs::read_to_string(&cfg_path)
            && let Ok(value) = text.parse::<toml::Value>()
            && let Some(tbl) = value.as_table()
        {
            // agent.default_provider / agent.model
            let mut default_provider = "bai".to_string();
            if let Some(agent) = tbl.get("agent").and_then(|a| a.as_table())
                && let Some(dp) = agent.get("default_provider").and_then(|v| v.as_str())
            {
                default_provider = dp.to_string();
            }
            // providers: [{name, base_url, model, api_key}, ...]
            if let Some(providers) = tbl.get("providers").and_then(|p| p.as_array()) {
                for p in providers {
                            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let base = p.get("base_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let model = p.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let key = p.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let key_env = p.get("api_key_env").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if name.is_empty() || base.is_empty() {
                                continue;
                            }
                            // Resolve the effective model: provider's own, else default.
                            let eff_model = if model.is_empty() { default_model.to_string() } else { model.clone() };
                            // Resolve the key: inline api_key, else the env var.
                            let resolved_key = if !key.is_empty() {
                                key
                            } else if !key_env.is_empty() {
                                std::env::var(&key_env).unwrap_or_default()
                            } else {
                                String::new()
                            };
                            // A provider with no key at all is unusable.
                            if resolved_key.is_empty() {
                                continue;
                            }
                            let slot = ModelSlot { provider: name.clone(), model: eff_model.clone() };
                            // Put the default provider's slot first.
                            if name == default_provider {
                                slots.insert(0, slot);
                            } else {
                                slots.push(slot);
                            }
                            probes.push(ProviderProbe {
                                name: name.clone(),
                                base_url: base,
                                key: resolved_key,
                                model: eff_model,
                            });
                        }
                    }
                }



        // If nothing parsed, single fallback slot.
        if slots.is_empty() {
            slots.push(ModelSlot { provider: "bai".into(), model: default_model.into() });
        }
        Self { slots, probes }
    }

    /// Fire a real (tiny) chat completion at every candidate provider and
    /// drop any that fail. Keeps the roster healthy: a provider with a stale
    /// key or a down endpoint is skipped instead of 401ing every child.
    async fn prune_dead(&mut self) {
        if self.probes.is_empty() {
            return;
        }
        // Probe each unique provider+model in parallel, with a short timeout.
        let mut alive: Vec<String> = Vec::new();
        let mut handles = Vec::new();
        for probe in &self.probes {
            let p = probe.clone();
            handles.push(tokio::spawn(async move {
                let ok = provider_ok(&p.base_url, &p.key, &p.model).await;
                (p.name, ok)
            }));
        }
        for h in handles {
            if let Ok((name, true)) = h.await
                && !alive.contains(&name)
            {
                alive.push(name);
            }
        }
        if alive.is_empty() {
            // Everything is down — keep the default slot so spawn still
            // attempts (the child surfaces the real error), rather than
            // returning an empty roster.
            self.slots.truncate(1);
            self.slots[0].model = self.probes.first().map(|p| p.model.clone())
                .unwrap_or_else(|| "deepseek-v4-flash".to_string());
            return;
        }
        self.slots.retain(|s| alive.contains(&s.provider));
        if self.slots.is_empty() {
            // No candidate slot survived but probes succeeded — build from probe list.
            let mut seen = std::collections::HashSet::new();
            for p in &self.probes {
                if alive.contains(&p.name) && seen.insert(p.name.clone()) {
                    self.slots.push(ModelSlot { provider: p.name.clone(), model: p.model.clone() });
                }
            }
        }
    }

    /// Assign a slot for the `idx`-th agent. Round-robin over the roster.
    fn slot_for(&self, idx: usize, model_override: Option<&str>) -> ModelSlot {
        if let Some(m) = model_override {
            // Override keeps the default provider but forces the model.
            return ModelSlot { provider: self.slots[0].provider.clone(), model: m.into() };
        }
        let slot = &self.slots[idx % self.slots.len()];
        slot.clone()
    }

    /// Human-readable summary of the roster for the tool output.
    fn describe(&self) -> String {
        let mut names: Vec<String> = Vec::new();
        for s in &self.slots {
            let n = format!("{}:{}", s.provider, s.model);
            if !names.contains(&n) {
                names.push(n);
            }
        }
        names.join(", ")
    }
}

/// Fire one tiny chat completion against a provider; true if it responds OK.
/// Used by `prune_dead` to skip broken providers before routing children.
/// Uses `max_tokens=16` — NOT 1 — because some providers (bai) reject
/// `max_tokens <= 2` with a 400, which would wrongly prune a healthy provider.
async fn provider_ok(base_url: &str, api_key: &str, model: &str) -> bool {
    if let Ok(client) = apex_provider::Client::new(base_url.to_string(), api_key.to_string()) {
        let msg = apex_types::Message::user("ping");
        tokio::time::timeout(
            std::time::Duration::from_secs(6),
            client.chat(model, &[msg], &[], Some(16)),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
    } else {
        false
    }
}

/// True if a sub-agent's collected output indicates a failure worth retrying:
/// timeout, non-zero exit, spawn/herdr/io/join errors. Used both for the live
/// "ok vs failed" monitor line and for the supervisor auto-retry pass.
fn is_failure(out: &str) -> bool {
    out.starts_with("timed out")
        || out.starts_with("exit ")
        || out.starts_with("spawn error")
        || out.starts_with("herdr error")
        || out.starts_with("io error")
        || out.starts_with("join error")
}

/// Remove TUI spinner frames and ANSI escape codes from child output so the
/// meaningful answer (usually at the tail) is readable. Keeps text lines.
///
/// Strategy: the TUI overwrites the same line with `\r` (spinner frames).
/// For each `\n`-delimited line, keep only the text AFTER the LAST `\r`
/// (the final frame / the answer that landed on that line), then drop
/// short spinner-only lines (phase labels + iter counters).
fn strip_spinner(text: &str) -> String {
    // 1. Drop ANSI escape sequences. CSI sequences end with a letter
    //    ([K = erase-line, [m = style, etc.), not just 'm'.
    let mut clean = String::new();
    let mut in_escape = false;
    for ch in text.chars() {
        if in_escape {
            // An escape is a CSI (ESC [ ... final-byte) or ESC <single>.
            // End it when we hit a final byte (letter) after the params.
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if ch == '\u{1b}' {
            in_escape = true;
            continue;
        }
        clean.push(ch);
    }
    // 2. Split into lines; within each line keep only the post-last-\r tail.
    let mut lines: Vec<String> = Vec::new();
    for raw in clean.split('\n') {
        let tail = match raw.rfind('\r') {
            Some(i) => &raw[i + 1..],
            None => raw,
        };
        let line = tail.trim().to_string();
        if line.is_empty() {
            continue;
        }
        // Drop spinner-only lines: phase labels with iter counters.
        let is_spinner = line.contains("…")
            || (line.contains("iter ") && line.contains('·'))
            || line.starts_with('✓')
            || line.starts_with('◎')
            || line.starts_with("⋯")
            || line.starts_with('●')
            || line.starts_with('◐')
            || line.starts_with("⣾")
            || line.starts_with('┈');
        if !is_spinner {
            lines.push(line);
        }
    }
    lines.join("\n")
}

/// Parse the `goals` argument. Accepts plain strings ("build the api"),
/// role/content objects ({ "goal": "…" }, { "content": … }, { "task": … },
/// { "prompt": … }, { "description": … }) and drops empties. Lenient on
/// purpose: models frequently emit objects on the first try, and erroring
/// just makes them re-issue the same format (burning attempts).
fn parse_goals(args: &Value) -> Vec<String> {
    let mut goals: Vec<String> = Vec::new();
    if let Some(arr) = args.get("goals").and_then(|g| g.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                goals.push(s.to_string());
            } else if let Some(obj) = v.as_object() {
                for key in ["goal", "content", "task", "prompt", "description"] {
                    if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
                        goals.push(s.to_string());
                        break;
                    }
                }
            }
        }
    }
    goals.retain(|g| !g.trim().is_empty());
    goals
}

pub struct SpawnTool {
    ctx: ToolContext,
}

impl SpawnTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }

    /// Spawn ONE sub-agent — either as a background process or as a Herdr
    /// tab (when HERDR_ENV=1). Returns (index, goal, output) so results stay
    /// labeled with their original agent number.
    #[allow(clippy::too_many_arguments)]
    async fn spawn_one(
        idx: usize,
        cmd: &str,
        goal: &str,
        model: &str,
        provider: &str,
        child_iters: u32,
        child_budget: Option<u64>,
        kill_list: &Arc<KillList>,
        use_herdr: bool,
        workspace_id: Option<String>,
    ) -> (usize, String, String) {
        // Build the args.
        let mut args: Vec<String> = vec![
            "chat".into(),
            "--model".into(),
            model.into(),
            // Pin the child to its assigned provider so the pool doesn't
            // fail over to a provider with a mismatched key.
            "--provider".into(),
            provider.into(),
            // Force full autonomy: spawned children cannot receive stdin
            // (they run in background Herdr panes), so they must never
            // pause waiting for user input.
            "--cap".into(),
            "--max-iterations".into(),
            child_iters.to_string(),
        ];
        if let Some(b) = child_budget {
            args.push("--budget-seconds".into());
            args.push(b.to_string());
        }
        args.push(goal.to_string());

        if use_herdr {
            // Herdr path: spawn in a new tab in the CURRENT workspace (the
            // one ByteAI is already running in — no new workspace is created).
            // The tab is labeled with a short slug of the goal so the
            // operator sees every agent working side by side.
            match crate::herdr::spawn_pane(idx, cmd, &args, AGENT_TIMEOUT, workspace_id.as_deref()).await {
                Ok((output, _pane_id)) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let combined = if output.status.success() {
                        stdout.to_string()
                    } else {
                        format!("exit {}:\n{}\n{}",
                            output.status.code().unwrap_or(-1), stdout, stderr)
                    };
                    (idx, goal.to_string(), combined)
                }
                Err(e) => (idx, goal.to_string(), format!("herdr error: {e}")),
            }
        } else {
            // Background process path (original).
            let child = match Command::new(cmd).args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => return (idx, goal.to_string(), format!("spawn error: {e}")),
            };
            let pid = child.id().unwrap_or(0);
            kill_list.register(pid);

            let out = timeout(
                std::time::Duration::from_secs(AGENT_TIMEOUT),
                child.wait_with_output(),
            ).await;

            if let Ok(Ok(_)) = &out {
                kill_list.deregister(pid);
            }

            match out {
                Ok(Ok(o)) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let combined = if o.status.success() {
                        stdout.to_string()
                    } else {
                        format!("exit {}:\n{}\n{}",
                            o.status.code().unwrap_or(-1), stdout, stderr)
                    };
                    (idx, goal.to_string(), combined)
                }
                Ok(Err(e)) => (idx, goal.to_string(), format!("io error: {e}")),
                Err(_) => (idx, goal.to_string(), "timed out".into()),
            }
        }
    }
}

impl Tool for SpawnTool {
    fn name(&self) -> &'static str {
        "spawn"
    }

    /// Sub-agents run real tasks (each bounded by AGENT_TIMEOUT) — the
    /// generic 300s tool cap would kill a legitimate multi-agent delegation
    /// mid-flight. The agent loop grants long-running tools a much larger
    /// timeout instead.
    fn long_running(&self) -> bool {
        true
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "spawn".into(),
            description: "Spawn up to 1000 parallel sub-agents, each running `byteai chat` with \
an isolated prompt. Each agent is assigned a MODEL from a ROUND-ROBIN roster across \
ALL configured providers — so no single provider endpoint gets overloaded. \
For example, with bai (cloud) + omniroute (local, 648 models), agents alternate \
bai→omniroute→bai→... spreading the parallel load. \
When running inside Herdr (HERDR_ENV=1), ByteAI does NOT create a new workspace: \
each sub-agent is spawned in its OWN new Herdr TAB directly under the CURRENT \
workspace (where ByteAI is already running) — one tab per agent, so the whole team \
works side by side, visible, inspectable, and killable \
in the Herdr TUI while they work. The main agent waits for ALL sub-agents, collects \
every result, and monitors each one's success/failure live (failed or timed-out \
children are retried once automatically). \
Each child gets its own independent iteration budget \
(config: agent.delegation_max_iterations, default 250). \
Use for parallel research, multi-file code generation, and multi-perspective review. \
Results return sorted by goal order. The main agent loop evaluates the aggregate \
output and can assign more tasks based on job magnitude.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "goals": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "1-1000 goals, one per sub-agent. EACH GOAL IS A PLAIN STRING (e.g. \"build the product API\"), NOT an object — pass the goal text directly as the array element. Example: {\"goals\": [\"write the auth module\", \"write the checkout page\"], \"max_parallel\": 4}. Each is classified for model routing."
                    },
                    "max_parallel": {
                        "type": "integer",
                        "description": "Max concurrent workers (default 5, max 1000)"
                    },
                    "model_hint": {
                        "type": "string",
                        "description": "Override model routing: force all sub-agents to use this model instead of auto-classification"
                    }
                },
                "required": ["goals"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let goals = parse_goals(&args);
            if goals.is_empty() {
                return ok_outcome("", "spawn",
                    "ERROR: `goals` must be an array of 1-1000 goal strings. Example:\n\
                     {\"goals\": [\"build the product API\", \"write the frontend\"], \"max_parallel\": 5}".to_string(),
                    started.elapsed().as_millis() as u64);
            }
            // Clamp max_parallel to 1..1000.
            let max_parallel = args
                .get("max_parallel")
                .and_then(|m| m.as_u64())
                .unwrap_or(5)
                .clamp(1, 1000) as usize;

            let model_override = args.get("model_hint").and_then(|m| m.as_str()).map(|s| s.to_string());

            // Locate byteai binary.
            let byteai = std::env::current_exe().ok()
                .unwrap_or_else(|| "byteai".into());

            let default_model = if ctx.default_model.is_empty() {
                "deepseek-v4-flash".to_string()
            } else {
                ctx.default_model.clone()
            };
            let child_iters = ctx.delegation_max_iterations.unwrap_or(250);
            let child_budget = ctx.run_budget_seconds;
            let use_herdr = crate::herdr::is_inside_herdr();

            // Inside Herdr, we do NOT create a new workspace: each sub-agent
            // is spawned as its own new tab directly under the CURRENT
            // workspace (the one ByteAI is already running in). The workspace
            // id comes from HERDR_WORKSPACE_ID; if it is absent, spawn_pane
            // falls back to the caller's workspace automatically.
            let project_ws = if use_herdr {
                crate::herdr::caller_workspace_id()
                    .map(|ws| (ws, "current".to_string()))
            } else {
                None
            };

            // Build the model router from ALL configured providers and
            // distribute agents round-robin so no single provider endpoint
            // gets overloaded (rate limits) by a large parallel spawn.
            // First prune dead providers (401 stale keys, down endpoints)
            // so a broken provider never 401-kills the children on its slots.
            let mut router = ModelRouter::from_config(&ctx.data_dir, &default_model);
            router.prune_dead().await;

            let semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel));
            let kill_list = Arc::new(KillList::default());
            let mut handles = Vec::new();

            for (idx, goal) in goals.iter().enumerate() {
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let cmd = byteai.clone();
                let g = goal.clone();
                let kl = kill_list.clone();
                // Assign a (provider, model) slot: round-robin across the
                // roster, or force the model_hint on the default provider.
                let slot = router.slot_for(idx, model_override.as_deref());
                let mdl = slot.model.clone();
                let prov = slot.provider.clone();
                let ws = project_ws.as_ref().map(|(ws_id, _)| ws_id.clone());

                handles.push(tokio::spawn(async move {
                    let _permit = permit;
                    Self::spawn_one(idx, &cmd.to_string_lossy(), &g, &mdl, &prov, child_iters, child_budget, &kl, use_herdr, ws).await
                }));
            }

            let mut results: Vec<(usize, String, String)> = Vec::new();
            let total = handles.len();
            // Live progress: surface each child's completion in the TUI chat
            // (process-wide tracing WARN → chat log), so a multi-minute
            // delegation never looks like a frozen spinner.
            tracing::warn!("[spawn] launching {total} sub-agent(s), {max_parallel} parallel | roster=[{}]", router.describe());
            let mut done_count = 0usize;
            for h in handles {
                match h.await {
                    Ok((i, g, out)) => {
                        done_count += 1;
                        let ok = !is_failure(&out);
                        tracing::warn!(
                            "[spawn] agent {i} finished ({done_count}/{total}) — {} · {}",
                            if ok { "ok" } else { "failed" },
                            g.chars().take(60).collect::<String>()
                        );
                        results.push((i, g, out));
                    }
                    Err(e) => {
                        done_count += 1;
                        tracing::warn!("[spawn] agent join error ({done_count}/{total}): {e}");
                        results.push((0, String::new(), format!("join error: {e}")));
                    }
                }
            }

            // Supervisor pass: re-run any failed/timed-out sub-agent ONCE so a
            // transient error (rate-limit, a dead provider mid-turn, a stuck
            // Herdr tab) self-heals instead of silently losing that goal's
            // work. Retries run in parallel too, sharing the same max_parallel
            // semaphore so they queue fairly behind the first wave.
            let failed: Vec<(usize, String)> = results.iter()
                .filter(|(_, _, out)| is_failure(out))
                .map(|(i, g, _)| (*i, g.clone()))
                .collect();
            let failed_count = failed.len();
            let mut retried = 0usize;
            if !failed.is_empty() {
                tracing::warn!("[spawn] supervisor: retrying {failed_count} failed sub-agent(s) once");
                let mut retry_handles = Vec::new();
                for (idx, goal) in failed {
                    let permit = semaphore.clone().acquire_owned().await.unwrap();
                    let cmd = byteai.clone();
                    let kl = kill_list.clone();
                    let slot = router.slot_for(idx, model_override.as_deref());
                    let mdl = slot.model.clone();
                    let prov = slot.provider.clone();
                    let ws = project_ws.as_ref().map(|(ws_id, _)| ws_id.clone());
                    retry_handles.push(tokio::spawn(async move {
                        let _permit = permit;
                        Self::spawn_one(idx, &cmd.to_string_lossy(), &goal, &mdl, &prov, child_iters, child_budget, &kl, use_herdr, ws).await
                    }));
                }
                let mut done2 = 0usize;
                for h in retry_handles {
                    match h.await {
                        Ok((i, g, out)) => {
                            done2 += 1;
                            let ok = !is_failure(&out);
                            tracing::warn!(
                                "[spawn] retry agent {i} finished ({done2}/{failed_count}) — {} · {}",
                                if ok { "ok" } else { "failed again" },
                                g.chars().take(60).collect::<String>()
                            );
                            if ok {
                                retried += 1;
                            }
                            // Replace the first-wave failure with the retry result.
                            if let Some(entry) = results.iter_mut().find(|(ii, gg, _)| *ii == i && *gg == g) {
                                entry.2 = out;
                            } else {
                                results.push((i, g, out));
                            }
                        }
                        Err(e) => {
                            done2 += 1;
                            tracing::warn!("[spawn] retry join error ({done2}/{failed_count}): {e}");
                        }
                    }
                }
            }
            drop(kill_list);

            results.sort_by_key(|r| r.0);
            let mut out = String::new();
            out.push_str(&format!(
                "spawned {} sub-agent(s), {} parallel{} | herdr={} | roster=[{}] | workspace={}{}\n",
                goals.len(), max_parallel,
                if goals.len() > max_parallel { format!(" (queued {})", goals.len().saturating_sub(max_parallel)) } else { String::new() },
                use_herdr,
                router.describe(),
                project_ws.as_ref().map(|(ws, name)| format!("{name} ({ws})")).unwrap_or_else(|| "none".to_string()),
                if retried > 0 { format!(" | supervisor-retried {retried}") } else { String::new() },
            ));
            for (i, goal, output) in &results {
                // Strip TUI spinner/ANSI noise, then show the meaningful tail.
                let clean = strip_spinner(output);
                let preview: String = clean.chars().rev().take(300).collect::<String>()
                    .chars().rev().collect();
                out.push_str(&format!("\n--- Agent {i} ---\nGoal: {goal}\nOutput: {preview}\n"));
            }
            ok_outcome("", "spawn", out, started.elapsed().as_millis() as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_code_goal() {
        assert_eq!(classify_goal("implement a rust function"), "code");
        assert_eq!(classify_goal("fix the bug in the api"), "code");
        assert_eq!(classify_goal("write tests for the module"), "code");
    }

    #[test]
    fn classify_reasoning_goal() {
        assert_eq!(classify_goal("why does this architecture fail"), "reasoning");
        assert_eq!(classify_goal("design a distributed system"), "reasoning");
        assert_eq!(classify_goal("analyze the trade-offs"), "reasoning");
    }

    #[test]
    fn classify_fast_goal() {
        assert_eq!(classify_goal("what is 2+2"), "fast");
        assert_eq!(classify_goal("reply with exactly one word"), "fast");
    }

    #[test]
    fn classify_memory_goal() {
        assert_eq!(classify_goal("recall what we decided"), "memory");
        assert_eq!(classify_goal("summarize the previous session"), "memory");
    }

    #[test]
    fn classify_default_fallback() {
        assert_eq!(classify_goal("hello world"), "default");
        assert_eq!(classify_goal("something completely random phrase"), "default");
    }

    #[test]
    fn pick_model_falls_back_to_default() {
        let m = pick_model("code", "my-default-model");
        // Should return something — either an OmniRoute alias or the default.
        assert!(!m.is_empty());
    }

    #[test]
    fn parse_goals_accepts_strings_objects_and_skips_empties() {
        // Plain strings.
        let g1 = parse_goals(&json!({"goals": ["build the api", "write the frontend"]}));
        assert_eq!(g1, vec!["build the api", "write the frontend"]);

        // The transcript's failure mode: role/content OBJECTS from the model.
        let g2 = parse_goals(&json!({"goals": [
            {"goal": "build the api"},
            {"content": "write the frontend"},
            {"task": "setup the database"},
            {"role": "backend", "goal": "auth module"}
        ]}));
        assert_eq!(g2, vec!["build the api", "write the frontend", "setup the database", "auth module"]);

        // Empties + non-string junk dropped; empty goals array → empty result.
        let g3 = parse_goals(&json!({"goals": ["", "   ", 42, {}, {"prompt": "ok"}]}));
        assert_eq!(g3, vec!["ok"]);

        // Missing/invalid goals → empty (triggers the friendly error).
        assert!(parse_goals(&json!({})).is_empty());
        assert!(parse_goals(&json!({"goals": "not-an-array"})).is_empty());
    }

    #[test]
    fn strip_spinner_keeps_answer_text() {
        // Simulate spinner output with answer on the last \r frame.
        let input = "\r  \u{22ef} \u{2b0b} UNDERSTANDING \u{2026} \u{b7} iter 1/5 \r  \u{2713} \u{25cf} COMPLETE \u{2026} \u{b7} iter 1/5 \rANSWER_TEXT_HERE\n\n[done: 1 iterations, 0 tool calls, 100 tokens, phase=COMPLETE]\n";
        let stripped = strip_spinner(input);
        assert!(stripped.contains("ANSWER_TEXT_HERE"), "answer text lost: {stripped:?}");
        assert!(!stripped.contains("iter 1/5"), "spinner survived: {stripped:?}");
    }

    #[test]
    fn model_router_reads_config_and_rotates() {
        // Build a temp data_dir with a config that has bai (cloud) + omniroute (local).
        let dir = std::env::temp_dir().join(format!("byteai_router_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = r#"
[agent]
model = "deepseek-v4-flash"
default_provider = "bai"

[[providers]]
name = "bai"
base_url = "https://api.b.ai/v1"
api_key = "sk-test"
model = "deepseek-v4-flash"

[[providers]]
name = "omniroute"
base_url = "http://localhost:20128/v1"
api_key = "sk-local"
model = ""
"#;
        std::fs::write(dir.join("config.toml"), cfg).unwrap();

        let router = ModelRouter::from_config(&dir, "deepseek-v4-flash");
        // Default provider first, then omniroute (no auto/best-* aliases).
        assert_eq!(router.slots[0].provider, "bai");
        assert_eq!(router.slots[0].model, "deepseek-v4-flash");
        assert_eq!(router.slots.len(), 2, "expected 2 provider slots, got {}", router.slots.len());

        // Round-robin: index 0 -> bai, index 1 -> omniroute, index 2 -> bai...
        let s0 = router.slot_for(0, None);
        let s1 = router.slot_for(1, None);
        let s2 = router.slot_for(2, None);
        assert_eq!(s0.provider, "bai");
        assert_eq!(s1.provider, "omniroute");
        assert_eq!(s2.provider, "bai", "slot 2 should cycle back to bai (len={})", router.slots.len());
        assert_ne!(s0.provider, s1.provider, "round-robin should alternate providers");

        // Override pins model on the default provider.
        let s_override = router.slot_for(7, Some("my-model"));
        assert_eq!(s_override.provider, "bai");
        assert_eq!(s_override.model, "my-model");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_failure_detects_all_failure_prefixes() {
        assert!(is_failure("timed out"));
        assert!(is_failure("exit 1:\nboom"));
        assert!(is_failure("spawn error: nope"));
        assert!(is_failure("herdr error: tab gone"));
        assert!(is_failure("io error: eof"));
        assert!(is_failure("join error: canceled"));
        // Success-ish outputs are not failures.
        assert!(!is_failure("done"));
        assert!(!is_failure("here is the answer"));
        assert!(!is_failure("exiting gracefully"));
        assert!(!is_failure("existence is fine"));
        // Prefix must be exact at the start (not just any substring).
        assert!(!is_failure("the exit code was 0"));
    }
}
