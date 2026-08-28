//! DAP (Debug Adapter Protocol) client for ByteAi (APEX) — Phase 4.
//!
//! A minimal-but-real DAP client over stdio, structurally parallel to
//! apex-lsp: spawn an adapter (debugpy, lldb-dap, gdb), speak JSON-RPC,
//! and expose the operations an agent needs:
//!   initialize → launch/attach → setBreakpoints → continue →
//!   stackTrace → scopes → variables → evaluate → disconnect
//!
//! DAP uses the same Content-Length framing as LSP; messages carry
//! `seq` (request id), `command`, `arguments`, `success`, `body`.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tracing::debug;

/// How long a request may wait for a response (DAP servers can be slow to
/// stop/step; breakpoint setting is usually fast).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const INIT_TIMEOUT: Duration = Duration::from_secs(10);

/// A debug adapter spec: command + args + how to launch programs.
#[derive(Debug, Clone)]
pub struct DapSpec {
    pub lang: String,
    pub command: String,
    pub args: Vec<String>,
    /// Language id passed to the adapter in `initialize`.
    pub adapter_id: String,
}

/// Known adapters; each is auto-detected on PATH.
pub fn default_adapters() -> Vec<DapSpec> {
    vec![
        DapSpec { lang: "python".into(), command: "python3".into(), args: vec!["-m".into(), "debugpy.adapter".into()], adapter_id: "debugpy".into() },
        DapSpec { lang: "c".into(), command: "lldb-dap".into(), args: vec![], adapter_id: "lldb-dap".into() },
        DapSpec { lang: "cpp".into(), command: "lldb-dap".into(), args: vec![], adapter_id: "lldb-dap".into() },
        DapSpec { lang: "rust".into(), command: "lldb-dap".into(), args: vec![], adapter_id: "lldb-dap".into() },
        DapSpec { lang: "node".into(), command: "node".into(), args: vec![], adapter_id: "node".into() },
    ]
}

/// One live adapter session (a debugee lifecycle).
pub struct DapSession {
    pub spec: DapSpec,
    child: Child,
    stdin: ChildStdin,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_seq: Arc<std::sync::atomic::AtomicU64>,
    /// adapter → client events that matter to an agent (stopped, output, ...)
    pub events: Arc<Mutex<Vec<DapEvent>>>,
    initialized: bool,
}

#[derive(Debug, Clone)]
pub struct DapEvent {
    pub kind: String, // "stopped" | "output" | "exited" | "terminated" | "breakpoint" | ...
    pub body: String, // human-readable payload
}

impl DapSession {
    /// Spawn the adapter and perform the DAP `initialize` handshake.
    pub async fn spawn(spec: DapSpec, _cwd: &Path) -> Result<Self> {
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("spawn DAP adapter {}", spec.command))?;
        let stdin = child.stdin.take().context("DAP adapter stdin")?;
        let stdout = child.stdout.take().context("DAP adapter stdout")?;
        let stderr = child.stderr.take().context("DAP adapter stderr")?;

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let next_seq = Arc::new(std::sync::atomic::AtomicU64::new(1));
        let events = Arc::new(Mutex::new(Vec::new()));

        let p2 = pending.clone();
        let ev2 = events.clone();
        let cmd_name = spec.command.clone();
        let mut reader = BufReader::new(stdout);
        let mut err_reader = BufReader::new(stderr);

        tokio::spawn(async move {
            // stderr → log
            let ecmd = cmd_name.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    match err_reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => debug!("[{ecmd} stderr] {}", line.trim_end()),
                    }
                }
            });
            // stdout → dispatch
            loop {
                let mut content_length: Option<usize> = None;
                let mut header_line = String::new();
                loop {
                    header_line.clear();
                    match reader.read_line(&mut header_line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let line = header_line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(rest) = line.strip_prefix("Content-Length:") {
                        content_length = rest.trim().parse::<usize>().ok();
                    }
                }
                let Some(len) = content_length else { return };
                let mut body = vec![0u8; len];
                if reader.read_exact(&mut body).await.is_err() {
                    return;
                }
                let Ok(msg): Result<Value, _> = serde_json::from_slice(&body) else { continue };
                let mtype = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match mtype {
                    "response" => {
                        if let Some(seq) = msg.get("request_seq").and_then(|v| v.as_u64())
                            && let Some(tx) = p2.lock().await.remove(&seq) {
                                let _ = tx.send(msg);
                            }
                    }
                    "event" => {
                        let kind = msg.get("event").and_then(|e| e.as_str()).unwrap_or("event").to_string();
                        let body = msg.get("body").cloned().unwrap_or(Value::Null);
                        let text = match kind.as_str() {
                            "output" => body.get("output").and_then(|o| o.as_str()).unwrap_or("").to_string(),
                            "stopped" => format!(
                                "{} @ {}:{}",
                                body.get("reason").and_then(|r| r.as_str()).unwrap_or("stopped"),
                                body.get("hitBreakpointIds").and_then(|b| b.as_array()).map(|a| format!("{:?}", a.iter().filter_map(|v| v.as_u64()).collect::<Vec<_>>())).unwrap_or_default(),
                                body.get("line").and_then(|l| l.as_u64()).unwrap_or(0),
                            ),
                            _ => serde_json::to_string(&body).unwrap_or_default(),
                        };
                        ev2.lock().await.push(DapEvent { kind, body: text });
                    }
                    _ => {}
                }
            }
        });

        let mut s = Self { spec: spec.clone(), child, stdin, pending, next_seq, events, initialized: false };
        s.initialize().await?;
        Ok(s)
    }

    async fn write_frame(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg).context("serialize DAP message")?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(&mut self, command: &str, args: Value) -> Result<Value> {
        let seq = self.next_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(seq, tx);
        let msg = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": args,
        });
        self.write_frame(&msg).await?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(resp)) => {
                if resp.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
                    Ok(resp.get("body").cloned().unwrap_or(Value::Null))
                } else {
                    let m = resp.get("message").and_then(|m| m.as_str()).unwrap_or("DAP request failed");
                    bail!("{command}: {m}")
                }
            }
            Ok(Err(_)) => bail!("{command}: channel closed"),
            Err(_) => bail!("{command}: timed out after {}s", REQUEST_TIMEOUT.as_secs()),
        }
    }

    #[allow(dead_code)]
    pub async fn notify(&mut self, command: &str, args: Value) -> Result<()> {
        let seq = self.next_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let msg = json!({ "seq": seq, "type": "request", "command": command, "arguments": args });
        self.write_frame(&msg).await
    }

    async fn initialize(&mut self) -> Result<()> {
        let args = json!({
            "adapterID": self.spec.adapter_id,
            "clientID": "byteai",
            "clientName": "ByteAi (APEX)",
            "columnsStartAt1": true,
            "linesStartAt1": true,
            "supportsVariableType": true,
            "supportsVariablePaging": false,
            "supportsTerminateRequest": true,
        });
        let _ = tokio::time::timeout(INIT_TIMEOUT, self.request("initialize", args)).await.map_err(|_| anyhow::anyhow!("DAP initialize timed out"))??;
        self.initialized = true;
        debug!("[{}] DAP adapter initialized", self.spec.command);
        Ok(())
    }

    /// `launch` a program (debugpy adapter: {target, cwd}; lldb-dap: {program, args, cwd}).
    /// After launch the adapter emits `initialized`; the client then sets
    /// breakpoints and sends `configurationDone` to start execution.
    pub async fn launch(&mut self, program: &str, args: Vec<String>, cwd: &str) -> Result<()> {
        let launch_args = match self.spec.lang.as_str() {
            "python" => json!({ "request": "launch", "type": "python", "program": program, "cwd": cwd, "justMyCode": false }),
            _ => json!({ "request": "launch", "program": program, "args": args, "cwd": cwd }),
        };
        let _ = self.request("launch", launch_args).await?;
        // Wait for the adapter's `initialized` event before configuring.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let ready = {
                let evs = self.events.lock().await;
                evs.iter().any(|e| e.kind == "initialized")
            };
            if ready {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("launch: adapter never sent `initialized`");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// Tell the adapter configuration is complete; program execution starts.
    pub async fn configuration_done(&mut self) -> Result<()> {
        let _ = self.request("configurationDone", json!({})).await?;
        Ok(())
    }

    pub async fn set_breakpoints(&mut self, path: &str, lines: &[u64]) -> Result<()> {
        let bps: Vec<Value> = lines.iter().map(|l| json!({ "line": l })).collect();
        let args = json!({ "source": { "path": path }, "breakpoints": bps, "sourceModified": false });
        let _ = self.request("setBreakpoints", args).await?;
        Ok(())
    }

    pub async fn continue_run(&mut self) -> Result<()> {
        let thread = self.threads().await?.first().cloned().unwrap_or(1);
        let _ = self.request("continue", json!({ "threadId": thread })).await?;
        Ok(())
    }

    pub async fn threads(&mut self) -> Result<Vec<u64>> {
        let body = self.request("threads", json!({})).await?;
        Ok(body
            .get("threads")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|t| t.get("id").and_then(|i| i.as_u64())).collect())
            .unwrap_or_default())
    }

    pub async fn stack_trace(&mut self, thread_id: u64, levels: u64) -> Result<Vec<StackFrame>> {
        let body = self.request("stackTrace", json!({ "threadId": thread_id, "levels": levels })).await?;
        Ok(parse_frames(body.get("stackFrames")))
    }

    pub async fn scopes(&mut self, frame_id: u64) -> Result<Vec<Value>> {
        let body = self.request("scopes", json!({ "frameId": frame_id })).await?;
        Ok(body.get("scopes").and_then(|s| s.as_array()).cloned().unwrap_or_default())
    }

    pub async fn variables(&mut self, vars_ref: u64) -> Result<Vec<Value>> {
        let body = self.request("variables", json!({ "variablesReference": vars_ref })).await?;
        Ok(body.get("variables").and_then(|v| v.as_array()).cloned().unwrap_or_default())
    }

    pub async fn evaluate(&mut self, expr: &str, frame_id: u64) -> Result<String> {
        let body = self.request("evaluate", json!({ "expression": expr, "frameId": frame_id, "context": "repl" })).await?;
        Ok(body.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string())
    }

    pub async fn next(&mut self, thread_id: u64) -> Result<()> {
        let _ = self.request("next", json!({ "threadId": thread_id })).await?;
        Ok(())
    }

    pub async fn step_in(&mut self, thread_id: u64) -> Result<()> {
        let _ = self.request("stepIn", json!({ "threadId": thread_id })).await?;
        Ok(())
    }

    pub async fn step_out(&mut self, thread_id: u64) -> Result<()> {
        let _ = self.request("stepOut", json!({ "threadId": thread_id })).await?;
        Ok(())
    }

    pub async fn pause(&mut self, thread_id: u64) -> Result<()> {
        let _ = self.request("pause", json!({ "threadId": thread_id })).await?;
        Ok(())
    }

    /// Wait for a `stopped` event (or timeout). Returns the reason.
    pub async fn wait_stopped(&self, timeout: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let evs = self.events.lock().await;
                for ev in evs.iter().rev() {
                    if ev.kind == "stopped" {
                        return Some(ev.body.clone());
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn drain_output(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut evs = self.events.lock().await;
        let mut i = 0;
        while i < evs.len() {
            if evs[i].kind == "output" {
                out.push(evs[i].body.clone());
                evs.remove(i);
            } else {
                i += 1;
            }
        }
        out
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        if self.initialized {
            let _ = self.request("disconnect", json!({ "terminateDebuggee": true })).await;
        }
        self.child.kill().await.ok();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub id: u64,
    pub name: String,
    pub source: String,
    pub line: u64,
    pub column: u64,
}

fn parse_frames(v: Option<&Value>) -> Vec<StackFrame> {
    let mut out = Vec::new();
    let Some(arr) = v.and_then(|a| a.as_array()) else { return out };
    for f in arr {
        out.push(StackFrame {
            id: f.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
            name: f.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
            source: f.get("source").and_then(|s| s.get("path")).and_then(|p| p.as_str()).unwrap_or("?").to_string(),
            line: f.get("line").and_then(|l| l.as_u64()).unwrap_or(0),
            column: f.get("column").and_then(|c| c.as_u64()).unwrap_or(0),
        });
    }
    out
}

// ────────────────────────────────────────────────────────────────────────────
// Registry: language → live session (or unavailable)
// ────────────────────────────────────────────────────────────────────────────

pub struct DapRegistry {
    specs: Vec<DapSpec>,
    sessions: tokio::sync::RwLock<HashMap<String, Arc<Mutex<DapState>>>>,
}

#[allow(clippy::large_enum_variant)]
pub enum DapState {
    Ready(DapSession),
    Unavailable(String),
    Spawning,
}

pub fn command_on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).exists();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.join(cmd).exists() {
                return true;
            }
        }
    }
    false
}

impl DapRegistry {
    pub fn new(specs: Vec<DapSpec>) -> Self {
        let specs: Vec<DapSpec> = specs.into_iter().filter(|s| command_on_path(&s.command)).collect();
        Self { specs, sessions: tokio::sync::RwLock::new(HashMap::new()) }
    }

    pub fn available(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.lang.clone()).collect()
    }

    pub fn supports(&self, lang: &str) -> bool {
        self.specs.iter().any(|s| s.lang == lang)
    }

    pub fn spec_for_lang(&self, lang: &str) -> Option<DapSpec> {
        self.specs.iter().find(|s| s.lang == lang).cloned()
    }

    pub async fn get(&self, lang: &str, cwd: &Path) -> Result<Arc<Mutex<DapState>>> {
        if let Some(state) = self.sessions.read().await.get(lang) {
            return Ok(state.clone());
        }
        let Some(spec) = self.spec_for_lang(lang) else {
            bail!("no DAP adapter configured for language {lang:?}");
        };
        let mut guard = self.sessions.write().await;
        if let Some(state) = guard.get(lang) {
            return Ok(state.clone());
        }
        let state = Arc::new(Mutex::new(DapState::Spawning));
        guard.insert(lang.to_string(), state.clone());
        drop(guard);

        let mut st = state.lock().await;
        match DapSession::spawn(spec.clone(), cwd).await {
            Ok(s) => {
                *st = DapState::Ready(s);
                debug!("[{lang}] DAP adapter ready");
            }
            Err(e) => {
                *st = DapState::Unavailable(format!("{e:#}"));
                debug!("[{lang}] DAP unavailable: {e:#}");
            }
        }
        Ok(state.clone())
    }
}
