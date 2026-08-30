//! `mcp` — MCP client tool (Model Context Protocol).
//! Top-10 core feature (the 2026 AAIF standard, 10k+ servers): connect to
//! MCP servers (stdio process or HTTP URL), list tools/call tools/resources
//! so ByteAi can tap the entire MCP ecosystem (Playwright browser, filesystem,
//! GitHub, Slack, PostgreSQL, etc.) without reimplementing them.
//!
//! Protocol: JSON-RPC 2.0 over stdio (local servers) or HTTP POST (remote).
//! Server config: `~/.byteai/mcp.json` — array of {name, command?, url?}.

use std::path::{Path, PathBuf};

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool};

/// Loaded MCP server config.
#[derive(serde::Deserialize, Clone, Default)]
struct MpcServerCfg {
    name: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

fn load_config(data_dir: &Path) -> Vec<MpcServerCfg> {
    let path = data_dir.join("mcp.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<MpcServerCfg>>(&s).ok())
        .unwrap_or_default()
}

pub struct McpTool {
    pub data_dir: PathBuf,
}

impl McpTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
    // Note: each call spawns/fresh-connects (stateless); no process caching.
}

impl Tool for McpTool {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mcp".into(),
            description: "MCP client: connect to stdio or HTTP MCP servers. Actions: list_servers, list_tools {server}, call {server, tool, args}, list_resources {server}. Config: ~/.byteai/mcp.json.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list_servers","list_tools","call","list_resources"], "description": "Action"},
                    "server": {"type": "string", "description": "Server name from config"},
                    "tool": {"type": "string", "description": "Tool name (for action=call)"},
                    "args": {"type": "object", "description": "Tool arguments (for action=call)"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let data_dir = self.data_dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("").to_string();
            let server = args.get("server").and_then(Value::as_str).unwrap_or("").to_string();
            let tool = args.get("tool").and_then(Value::as_str).unwrap_or("").to_string();
            let tool_args = args.get("args").cloned().unwrap_or(Value::Null);

            let servers = load_config(&data_dir);
            if servers.is_empty() {
                return crate::ok_outcome("", "mcp", "No MCP servers configured. Create ~/.byteai/mcp.json with [{name, command/url, args?}]", started.elapsed().as_millis() as u64);
            }

            match action.as_str() {
                "list_servers" => {
                    let list: Vec<Value> = servers.iter().map(|s| json!({
                        "name": s.name,
                        "command": s.command,
                        "url": s.url,
                        "args": s.args,
                    })).collect();
                    crate::ok_outcome("", "mcp", json!({"servers": list}).to_string(), started.elapsed().as_millis() as u64)
                }
                "list_tools" | "list_resources" => {
                    let cfg = servers.iter().find(|s| s.name == server);
                    let cfg = match cfg {
                        Some(c) => c,
                        None => return crate::ok_outcome("", "mcp", format!("Server '{server}' not found in config"), started.elapsed().as_millis() as u64),
                    };
                    let req = if action == "list_tools" {
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
                    } else {
                        json!({"jsonrpc":"2.0","id":1,"method":"resources/list"})
                    };
                    match mcp_request(cfg, req).await {
                        Ok(resp) => crate::ok_outcome("", "mcp", resp.to_string(), started.elapsed().as_millis() as u64),
                        Err(e) => crate::ok_outcome("", "mcp", format!("MCP error: {e:#}"), started.elapsed().as_millis() as u64),
                    }
                }
                "call" => {
                    let cfg = servers.iter().find(|s| s.name == server);
                    let cfg = match cfg {
                        Some(c) => c,
                        None => return crate::ok_outcome("", "mcp", format!("Server '{server}' not found"), started.elapsed().as_millis() as u64),
                    };
                    let req = json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": tool, "arguments": tool_args}
                    });
                    match mcp_request(cfg, req).await {
                        Ok(resp) => crate::ok_outcome("", "mcp", resp.to_string(), started.elapsed().as_millis() as u64),
                        Err(e) => crate::ok_outcome("", "mcp", format!("MCP error: {e:#}"), started.elapsed().as_millis() as u64),
                    }
                }
                other => crate::ok_outcome("", "mcp", format!("unknown action '{other}'"), started.elapsed().as_millis() as u64),
            }
        })
    }
}

/// Send a JSON-RPC request to an MCP server (stdio or HTTP).
async fn mcp_request(cfg: &MpcServerCfg, req: Value) -> anyhow::Result<Value> {
    if let Some(url) = &cfg.url {
        // HTTP POST transport (simplified; full SSE not implemented).
        let client = reqwest::Client::new();
        let resp = client.post(url)
            .header("Content-Type", "application/json")
            .json(&req)
            .send()
            .await?
            .json::<Value>()
            .await?;
        Ok(resp)
    } else if let Some(cmd) = &cfg.command {
        // STDIO transport: spawn process, write request, read response.
        let mut child = tokio::process::Command::new(cmd)
            .args(&cfg.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        // Write the request.
        let mut req_str = req.to_string();
        req_str.push('\n');
        use tokio::io::AsyncWriteExt;
        stdin.write_all(req_str.as_bytes()).await?;
        drop(stdin); // EOF

        // Read the response.
        use tokio::io::AsyncReadExt;
        let mut buf = String::new();
        stdout.read_to_string(&mut buf).await?;
        let _ = child.wait().await;

        // Parse JSON-RPC response (may be single-line JSON or multi-line).
        let json: Value = serde_json::from_str(&buf)?;

        // Check for protocol-level error.
        if let Some(err) = json.get("error") {
            anyhow::bail!("MCP error: {}", err.get("message").and_then(Value::as_str).unwrap_or("unknown"));
        }
        Ok(json)
    } else {
        anyhow::bail!("server config must have 'command' or 'url'");
    }
}