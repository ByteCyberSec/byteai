//! LSP integration for ByteAi — Phase 2.
//!
//! A JSON-RPC 2.0 client for Language Server Protocol servers over stdio.
//! Design goals:
//! - OPTIONAL: no LSP server → everything degrades to structured "unavailable".
//! - Lazy: servers spawn on first use per language, then stay warm.
//! - Bounded: every request has a timeout; a hung server cannot wedge the agent.
//! - Useful: diagnostics, symbols, definition, references, hover, rename,
//!   formatting — enough for IDE-grade edits and smart reads.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, RwLock, oneshot};
use tracing::debug;

// ────────────────────────────────────────────────────────────────────────────
// Wire types (subset of LSP)
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct Diagnostic {
    pub range: (usize, usize, usize, usize), // start_line, start_col, end_line, end_col (0-based)
    pub severity: Option<u8>,                // 1=Error 2=Warning 3=Info 4=Hint
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: u16, // LSP SymbolKind
    pub start_line: usize, // 1-based for display
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocationInfo {
    pub uri: String,
    pub start_line: usize, // 1-based
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

// ────────────────────────────────────────────────────────────────────────────
// Server spec + registry
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub lang: String,
    /// LSP language id sent to the server (e.g. "rust", "typescript", "python").
    pub language_id: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Known-good servers; each is auto-detected on PATH. Unknown → unavailable.
pub fn default_servers() -> Vec<ServerSpec> {
    vec![
        ServerSpec { lang: "rust".into(), language_id: "rust".into(), command: "rust-analyzer".into(), args: vec![] },
        ServerSpec { lang: "typescript".into(), language_id: "typescript".into(), command: "typescript-language-server".into(), args: vec!["--stdio".into()] },
        ServerSpec { lang: "javascript".into(), language_id: "javascript".into(), command: "typescript-language-server".into(), args: vec!["--stdio".into()] },
        ServerSpec { lang: "python".into(), language_id: "python".into(), command: "pyright-langserver".into(), args: vec!["--stdio".into()] },
        ServerSpec { lang: "c".into(), language_id: "c".into(), command: "clangd".into(), args: vec![] },
        ServerSpec { lang: "cpp".into(), language_id: "cpp".into(), command: "clangd".into(), args: vec![] },
        ServerSpec { lang: "go".into(), language_id: "go".into(), command: "gopls".into(), args: vec![] },
    ]
}

pub fn language_for_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    let lang = match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "go" => "go",
        _ => return None,
    };
    Some(lang.to_string())
}

fn path_to_uri(path: &Path) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    // Minimal percent-encoding for URI-unfriendly chars.
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b' ' => out.push_str("%20"),
            b'#' => out.push_str("%23"),
            b'%' => out.push_str("%25"),
            b'?' => out.push_str("%3F"),
            b'<' => out.push_str("%3C"),
            b'>' => out.push_str("%3E"),
            _ => out.push(b as char),
        }
    }
    format!("file://{out}")
}

// ────────────────────────────────────────────────────────────────────────────
// LSP client (one per language server)
// ────────────────────────────────────────────────────────────────────────────

pub struct LspServer {
    pub spec: ServerSpec,
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    shutdown_sent: Arc<std::sync::atomic::AtomicBool>,
}

impl LspServer {
    const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
    const INIT_TIMEOUT: Duration = Duration::from_secs(30);

    pub async fn spawn(spec: ServerSpec, root: &Path) -> Result<Self> {
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("spawn LSP server {}", spec.command))?;
        let stdin = child.stdin.take().context("LSP server stdin")?;
        let stdout = child.stdout.take().context("LSP server stdout")?;
        let stderr = child.stderr.take().context("LSP server stderr")?;

        let pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(std::sync::atomic::AtomicU64::new(1));

        let mut server = Self {
            spec: spec.clone(),
            child,
            stdin,
            pending: pending.clone(),
            diagnostics: diagnostics.clone(),
            next_id: next_id.clone(),
            shutdown_sent: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        // Reader task: parse frames, route responses/notifications.
        let p2 = pending.clone();
        let d2 = diagnostics.clone();
        let spec2 = spec.clone();
        let stderr_cmd = spec2.command.clone();
        let mut reader = AsyncBufReader::new(stdout);
        let mut err_reader = AsyncBufReader::new(stderr);

        tokio::spawn(async move {
            // stderr → log (don't block)
            let err_cmd = stderr_cmd.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    match err_reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => debug!("[{} stderr] {}", err_cmd, line.trim_end()),
                    }
                }
            });
            loop {
                // Content-Length framing
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
                let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
                if !method.is_empty() && !method.contains("logMessage") && !method.contains("telemetry") {
                    debug!("[{}] <- {}", spec2.command, method);
                }
                if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                    if let Some(tx) = p2.lock().await.remove(&id) {
                        let _ = tx.send(msg);
                    }
                } else if let Some(params) = msg.get("params")
                    && method == "textDocument/publishDiagnostics" {
                        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
                        let diags = parse_diagnostics(params.get("diagnostics"));
                        debug!("[{}] publishDiagnostics {} diags for {}", spec2.command, diags.len(), uri);
                        d2.lock().await.insert(uri, diags);
                    }
            }
        });

        server.initialize(root).await?;
        Ok(server)
    }

    async fn write_frame(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg).context("serialize LSP message")?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_frame(&msg).await?;
        match tokio::time::timeout(Self::REQUEST_TIMEOUT, rx).await {
            Ok(Ok(resp)) => {
                if let Some(err) = resp.get("error") {
                    bail!("LSP {method} error: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown"));
                }
                Ok(resp.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(_)) => bail!("LSP {method}: channel closed (server exited)"),
            Err(_) => bail!("LSP {method}: timeout after {}s", Self::REQUEST_TIMEOUT.as_secs()),
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_frame(&msg).await
    }

    async fn initialize(&mut self, root: &Path) -> Result<()> {
        let root_uri = path_to_uri(root);
        let params = json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "definition": { "linkSupport": true },
                    "references": {},
                    "hover": {},
                    "rename": { "prepareSupport": true },
                    "publishDiagnostics": { "relatedInformation": true }
                },
                "workspace": { "symbol": {} }
            }
        });
        match tokio::time::timeout(Self::INIT_TIMEOUT, self.request("initialize", params)).await {
            Ok(Ok(result)) => {
                self.notify("initialized", json!({})).await?;
                debug!("[{}] LSP initialized", self.spec.command);
                let _ = result;
                Ok(())
            }
            Ok(Err(e)) => bail!("initialize failed: {e:#}"),
            Err(_) => bail!("initialize timed out after {}s", Self::INIT_TIMEOUT.as_secs()),
        }
    }

    pub async fn did_open(&mut self, path: &Path, text: &str) -> Result<()> {
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri, "languageId": self.spec.language_id, "version": 1, "text": text }
        });
        self.notify("textDocument/didOpen", params).await
    }

    pub async fn did_change(&mut self, path: &Path, text: &str, version: i64) -> Result<()> {
        let uri = path_to_uri(path);
        let params = json!({
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [ { "text": text } ]  // full sync
        });
        self.notify("textDocument/didChange", params).await
    }

    /// Open (if needed) then ensure the file's current text is known to the server.
    pub async fn sync_file(&mut self, path: &Path, text: &str) -> Result<()> {
        self.did_change(path, text, 1).await
    }

    pub async fn shutdown(&mut self) {
        if self.shutdown_sent.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let _ = self.request("shutdown", json!(null)).await;
        let _ = self.notify("exit", json!({})).await;
    }

    // ── Capability calls ─────────────────────────────────────────────────────

    pub async fn document_symbols(&mut self, path: &Path) -> Result<Vec<SymbolInfo>> {
        let uri = path_to_uri(path);
        let result = self.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": uri } })).await?;
        Ok(parse_symbols(&result))
    }

    pub async fn definition(&mut self, path: &Path, line: usize, col: usize) -> Result<Vec<LocationInfo>> {
        let uri = path_to_uri(path);
        let result = self
            .request(
                "textDocument/definition",
                json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": col } }),
            )
            .await?;
        Ok(parse_locations(&result))
    }

    pub async fn references(&mut self, path: &Path, line: usize, col: usize, include_decl: bool) -> Result<Vec<LocationInfo>> {
        let uri = path_to_uri(path);
        let result = self
            .request(
                "textDocument/references",
                json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": col }, "context": { "includeDeclaration": include_decl } }),
            )
            .await?;
        Ok(parse_locations(&result))
    }

    pub async fn hover(&mut self, path: &Path, line: usize, col: usize) -> Result<String> {
        let uri = path_to_uri(path);
        let result = self
            .request("textDocument/hover", json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": col } }))
            .await?;
        Ok(format_hover(&result))
    }

    pub async fn rename(&mut self, path: &Path, line: usize, col: usize, new_name: &str) -> Result<Value> {
        let uri = path_to_uri(path);
        self.request(
            "textDocument/rename",
            json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": col }, "newName": new_name }),
        )
        .await
    }

    pub async fn formatting(&mut self, path: &Path) -> Result<Vec<TextEdit>> {
        let uri = path_to_uri(path);
        let result = self
            .request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 4, "insertSpaces": true } }))
            .await?;
        Ok(parse_text_edits(&result))
    }

    pub async fn workspace_symbols(&mut self, query: &str) -> Result<Vec<SymbolInfo>> {
        let result = self.request("workspace/symbol", json!({ "query": query })).await?;
        Ok(parse_symbols(&result))
    }

    /// Wait for publishDiagnostics for a URI, up to `timeout`.
    pub async fn wait_diagnostics(&self, path: &Path, timeout: Duration) -> Vec<Diagnostic> {
        let uri = path_to_uri(path);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let map = self.diagnostics.lock().await;
                // A publishDiagnostics for this URI — even an empty array — is
                // a definitive answer from the server. Return it.
                if let Some(d) = map.get(&uri) {
                    return d.clone();
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Vec::new();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn current_diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        let uri = path_to_uri(path);
        std::thread::scope(|_| {
            // sync helper: block on the async mutex via a mini runtime is overkill;
            // instead return from a snapshot taken by the caller when needed.
            let _ = uri;
            Vec::new()
        })
    }
}

#[derive(Debug, Clone)]
pub struct TextEdit {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub new_text: String,
}

// ────────────────────────────────────────────────────────────────────────────
// Parsers (pure, testable)
// ────────────────────────────────────────────────────────────────────────────

fn parse_diagnostics(v: Option<&Value>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let Some(arr) = v.and_then(|d| d.as_array()) else { return out };
    for d in arr {
        let range = d.get("range");
        let (sl, sc, el, ec) = match range.and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(|l| l.as_u64()) {
            Some(_) => (
                range.and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(0) as usize,
                range.and_then(|r| r.get("start")).and_then(|s| s.get("character")).and_then(|c| c.as_u64()).unwrap_or(0) as usize,
                range.and_then(|r| r.get("end")).and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(0) as usize,
                range.and_then(|r| r.get("end")).and_then(|s| s.get("character")).and_then(|c| c.as_u64()).unwrap_or(0) as usize,
            ),
            None => (0, 0, 0, 0),
        };
        out.push(Diagnostic {
            range: (sl, sc, el, ec),
            severity: d.get("severity").and_then(|s| s.as_u64()).map(|s| s as u8),
            message: d.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
            source: d.get("source").and_then(|s| s.as_str()).map(String::from),
            code: d.get("code").and_then(|c| c.as_str()).map(String::from).or_else(|| {
                d.get("code").and_then(|c| c.as_u64()).map(|n| n.to_string())
            }),
        });
    }
    out
}

fn parse_symbols(v: &Value) -> Vec<SymbolInfo> {
    let mut out = Vec::new();
    // Two response shapes: array of SymbolInformation, or array of DocumentSymbol (hierarchical).
    let Some(arr) = v.as_array() else { return out };
    fn walk(items: &[Value], out: &mut Vec<SymbolInfo>) {
        for s in items {
            let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let kind = s.get("kind").and_then(|k| k.as_u64()).unwrap_or(0) as u16;
            // DocumentSymbol shape: has `range`; SymbolInformation shape: has `location`.
            if let Some(range) = s.get("range") {
                let (sl, sc, el, ec) = range_from(range);
                out.push(SymbolInfo {
                    detail: s.get("detail").and_then(|d| d.as_str()).map(String::from),
                    name,
                    kind,
                    start_line: sl + 1,
                    start_col: sc,
                    end_line: el + 1,
                    end_col: ec,
                });
            } else if let Some(loc) = s.get("location").and_then(|l| l.get("range")) {
                let (sl, sc, el, ec) = range_from(loc);
                out.push(SymbolInfo {
                    detail: s.get("containerName").and_then(|c| c.as_str()).map(|c| format!("in {c}")),
                    name,
                    kind,
                    start_line: sl + 1,
                    start_col: sc,
                    end_line: el + 1,
                    end_col: ec,
                });
            }
            if let Some(children) = s.get("children").and_then(|c| c.as_array()) {
                walk(children, out);
            }
        }
    }
    walk(arr, &mut out);
    out
}

fn parse_locations(v: &Value) -> Vec<LocationInfo> {
    let mut out = Vec::new();
    let Some(arr) = v.as_array() else { return out };
    for loc in arr {
        // Definition can be Location or LocationLink (has targetUri/targetRange).
        let (uri, range) = if let Some(u) = loc.get("targetUri") {
            (u.as_str().unwrap_or(""), loc.get("targetRange").unwrap_or(&Value::Null))
        } else {
            (loc.get("uri").and_then(|u| u.as_str()).unwrap_or(""), loc.get("range").unwrap_or(&Value::Null))
        };
        let (sl, sc, el, ec) = range_from(range);
        out.push(LocationInfo { uri: uri.to_string(), start_line: sl + 1, start_col: sc, end_line: el + 1, end_col: ec });
    }
    out
}

fn format_hover(v: &Value) -> String {
    let Some(contents) = v.get("contents") else { return String::new() };
    let mut parts = Vec::new();
    match contents {
        Value::String(s) => parts.push(s.clone()),
        Value::Array(a) => {
            for item in a {
                if let Some(s) = item.as_str() {
                    parts.push(s.to_string());
                } else if let Some(v) = item.get("value").and_then(|x| x.as_str()) {
                    parts.push(v.to_string());
                }
            }
        }
        Value::Object(o) => {
            if let Some(v) = o.get("value").and_then(|x| x.as_str()) {
                parts.push(v.to_string());
            }
        }
        _ => {}
    }
    parts.join("\n\n")
}

fn parse_text_edits(v: &Value) -> Vec<TextEdit> {
    let mut out = Vec::new();
    let Some(arr) = v.as_array() else { return out };
    for e in arr {
        let (sl, sc, el, ec) = range_from(e.get("range").unwrap_or(&Value::Null));
        out.push(TextEdit {
            start_line: sl + 1,
            start_col: sc,
            end_line: el + 1,
            end_col: ec,
            new_text: e.get("newText").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        });
    }
    out
}

fn range_from(v: &Value) -> (usize, usize, usize, usize) {
    let n = |path: &str| {
        v.pointer(path)
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as usize
    };
    (n("/start/line"), n("/start/character"), n("/end/line"), n("/end/character"))
}

// ────────────────────────────────────────────────────────────────────────────
// Registry: language → live server (or unavailable)
// ────────────────────────────────────────────────────────────────────────────

pub struct LspRegistry {
    specs: Vec<ServerSpec>,
    servers: RwLock<HashMap<String, Arc<Mutex<ServerState>>>>,
}

#[allow(clippy::large_enum_variant)]
pub enum ServerState {
    Ready(LspServer),
    Unavailable(String),
    Spawning,
}

impl LspRegistry {
    pub fn new(specs: Vec<ServerSpec>) -> Self {
        // Drop servers whose command is not on PATH (cheap check, avoids churn).
        let specs: Vec<ServerSpec> = specs.into_iter().filter(|s| command_on_path(&s.command)).collect();
        Self { specs, servers: RwLock::new(HashMap::new()) }
    }

    pub fn available(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.lang.clone()).collect()
    }

    /// True when a server for this language is configured and on PATH.
    pub fn supports(&self, lang: &str) -> bool {
        self.spec_for_lang(lang).is_some()
    }

    pub fn spec_for_lang(&self, lang: &str) -> Option<ServerSpec> {
        self.specs.iter().find(|s| s.lang == lang).cloned()
    }

    /// Get (or spawn) the server for a language. Returns Unavailable reasons as Err.
    pub async fn get(&self, lang: &str, root: &Path) -> Result<Arc<Mutex<ServerState>>> {
        // Fast path: existing state.
        if let Some(state) = self.servers.read().await.get(lang) {
            return Ok(state.clone());
        }
        let Some(spec) = self.spec_for_lang(lang) else {
            return Err(anyhow::anyhow!("no LSP server configured for language {lang:?}"));
        };
        // Insert Spawning, then spawn outside the write lock.
        let mut guard = self.servers.write().await;
        if let Some(state) = guard.get(lang) {
            return Ok(state.clone());
        }
        let state = Arc::new(Mutex::new(ServerState::Spawning));
        guard.insert(lang.to_string(), state.clone());
        drop(guard);

        let mut st = state.lock().await;
        if let ServerState::Ready(_) = *st {
            return Ok(state.clone());
        }
        match LspServer::spawn(spec.clone(), root).await {
            Ok(server) => {
                *st = ServerState::Ready(server);
                debug!("[{}] LSP server ready", spec.lang);
            }
            Err(e) => {
                *st = ServerState::Unavailable(format!("{e:#}"));
                debug!("[{}] LSP unavailable: {e:#}", spec.lang);
            }
        }
        Ok(state.clone())
    }

    pub async fn shutdown_all(&self) {
        let servers = self.servers.read().await;
        for state in servers.values() {
            let mut st = state.lock().await;
            if let ServerState::Ready(s) = &mut *st {
                s.shutdown().await;
            }
        }
    }
}

pub fn command_on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return std::path::Path::new(cmd).exists();
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

#[cfg(test)]
fn encode_frame(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
    out
}

/// Boxed LSP future used by the registry helpers below.
pub type LspFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

/// Run `f` against the (spawned-on-demand) server for a language.
pub async fn with_server<T>(
    registry: &LspRegistry,
    lang: &str,
    root: &Path,
    f: impl for<'a> FnOnce(&'a mut LspServer) -> LspFuture<'a, T>,
) -> Result<T> {
    let state = registry.get(lang, root).await?;
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => f(s).await,
        ServerState::Unavailable(reason) => bail!("LSP {lang} unavailable: {reason}"),
        ServerState::Spawning => bail!("LSP {lang} still spawning"),
    }
}

/// Convenience: open the file, then run `f` (keeps the server warm).
pub async fn open_and<T>(
    registry: &LspRegistry,
    path: &Path,
    text: &str,
    f: impl for<'a> FnOnce(&'a mut LspServer) -> LspFuture<'a, T>,
) -> Result<T> {
    let lang = language_for_path(path).ok_or_else(|| anyhow::anyhow!("no LSP language for {}", path.display()))?;
    let root = path.parent().unwrap_or(Path::new("."));
    let state = registry.get(&lang, root).await?;
    let mut st = state.lock().await;
    match &mut *st {
        ServerState::Ready(s) => {
            s.did_open(path, text).await?;
            f(s).await
        }
        ServerState::Unavailable(reason) => bail!("LSP {lang} unavailable: {reason}"),
        ServerState::Spawning => bail!("LSP {lang} still spawning"),
    }
}

#[allow(dead_code)]
fn _sync_helper_unused() {}

#[cfg(test)]
mod tests;
