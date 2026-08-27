//! `byteai serve` — HTTP daemon that exposes tools + chat routing over HTTP.
//!
//! Two modes in one:
//!   * **Router** — POST /v1/chat/completions proxies to the configured provider
//!     (OmniRoute, Ollama, etc.), making ByteAI a drop-in OpenAI-compatible
//!     endpoint for any other agent on the machine.
//!   * **Daemon** — POST /v1/tools/<name> lets any HTTP client invoke ByteAI's
//!     built-in tools remotely (useful for CI hooks, cron jobs, other agents).
//!
//! Minimal HTTP/1.1 server on raw tokio — no new dependencies.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use apex_provider::Client;
use apex_tools::{Registry, ToolContext};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

/// ServeArgs parsed from the CLI.
pub struct ServeArgs {
    pub port: u16,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

/// Run the HTTP server until Ctrl-C.
pub async fn run(args: ServeArgs, ctx: ToolContext, registry: Registry) -> Result<()> {
    let addr: SocketAddr = ([127, 0, 0, 1], args.port).into();
    let listener = TcpListener::bind(addr).await?;
    info!("serve listening on http://{addr}");

    // Build the outbound client for chat-completions proxy.
    let client = match &ctx.client {
        Some(c) => Some(c.clone()),
        None => {
            // If no provider was configured, try to build one from args.
            if let (Some(url), Some(key)) = (&args.base_url, &args.api_key) {
                Client::new(url, key).ok()
            } else {
                None
            }
        }
    };
    let client = Arc::new(client);
    let registry = Arc::new(registry);
    let default_model = Arc::new(ctx.default_model.clone());

    loop {
        match listener.accept().await {
            Ok((mut stream, peer)) => {
                let client = client.clone();
                let reg = registry.clone();
                let dm = default_model.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = match stream.read(&mut buf).await {
                        Ok(n) if n > 0 => n,
                        Ok(_) => return,
                        Err(e) => {
                            warn!("serve read error from {peer}: {e}");
                            return;
                        }
                    };
                    let req = &buf[..n];
                    if let Err(e) = handle_request(req, &mut stream, &client, &reg, &dm).await {
                        let _ = write_response(&mut stream, 500, "text/plain", &format!("internal error: {e:#}")).await;
                    }
                });
            }
            Err(e) => {
                warn!("serve accept error: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Parse the first line + headers, route to handler.
async fn handle_request(
    raw: &[u8],
    stream: &mut tokio::net::TcpStream,
    client: &Option<Client>,
    registry: &Registry,
    default_model: &str,
) -> Result<()> {
    let s = std::str::from_utf8(raw)?;
    let mut lines = s.lines();
    let request_line = lines.next().unwrap_or("GET / HTTP/1.1");
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return write_response(stream, 400, "text/plain", "bad request line").await;
    }
    let method = parts[0];
    let path = parts[1];

    // Read headers to find Content-Length, then read the body.
    let mut content_length = 0usize;
    let mut body_start = 0usize;
    if let Some(double_crlf) = s.find("\r\n\r\n") {
        for hdr in s[..double_crlf].lines().skip(1) {
            if let Some(val) = hdr.strip_prefix("Content-Length:") {
                content_length = val.trim().parse().unwrap_or(0);
            }
        }
        body_start = double_crlf + 4;
    }
    let body = if content_length > 0 && body_start + content_length <= raw.len() {
        String::from_utf8_lossy(&raw[body_start..body_start + content_length]).to_string()
    } else {
        String::new()
    };

    // Route.
    match (method, path) {
        ("GET", "/health") => {
            write_response(stream, 200, "application/json", r#"{"status":"ok","service":"byteai"}"#).await
        }
        ("GET", "/v1/models") => {
            let models = match client {
                Some(c) => c.list_models().await.unwrap_or_default(),
                None => vec![default_model.to_string()],
            };
            let data: Vec<Value> = models.into_iter().map(|id| serde_json::json!({"id": id, "object": "model"})).collect();
            let resp = serde_json::json!({"object": "list", "data": data});
            write_response(stream, 200, "application/json", &resp.to_string()).await
        }
        ("GET", "/tools") => {
            let defs = registry.defs();
            let resp = serde_json::json!({"tools": defs});
            write_response(stream, 200, "application/json", &resp.to_string()).await
        }
        ("POST", "/v1/chat/completions") => {
            let json_body: Value = serde_json::from_str(&body)?;
            let model = json_body.get("model").and_then(Value::as_str).unwrap_or(default_model);
            let msgs = json_body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
            let apex_msgs: Vec<apex_types::Message> = msgs.iter()
                .filter_map(|m| {
                    let role = m.get("role")?.as_str()?;
                    let content = m.get("content")?.as_str()?;
                    Some(match role {
                        "system" => apex_types::Message::system(content),
                        "user" => apex_types::Message::user(content),
                        "assistant" => apex_types::Message::assistant(Some(content.into()), None, None),
                        _ => return None,
                    })
                })
                .collect();
            // Resolve the model to use: prefer the configured default_model (which
            // is already resolved by the CLI), or fall back to the request's model.
            let target_model = if !default_model.is_empty() && default_model != "auto/best-chat" {
                default_model
            } else {
                model
            };
            let (content, _tcalls, _usage) = match client {
                Some(c) => c.chat(target_model, &apex_msgs, &[], None).await?,
                None => ("no provider configured".to_string(), vec![], Default::default()),
            };
            let resp = serde_json::json!({
                "id": "chatcmpl-byteai",
                "object": "chat.completion",
                "model": target_model,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
            });
            write_response(stream, 200, "application/json", &resp.to_string()).await
        }
        (method, path) if path.starts_with("/v1/tools/") && method == "POST" => {
            let tool_name = path.trim_start_matches("/v1/tools/");
            let tool = match registry.get(tool_name) {
                Some(t) => t,
                None => return write_response(stream, 404, "text/plain", &format!("unknown tool: {tool_name}")).await,
            };
            let args: Value = if body.is_empty() { serde_json::json!({}) } else { serde_json::from_str(&body)? };
            let outcome = tool.execute(args).await;
            let resp = serde_json::json!({
                "tool": tool_name,
                "ok": outcome.ok,
                "output": outcome.output,
                "elapsed_ms": outcome.elapsed_ms,
            });
            write_response(stream, 200, "application/json", &resp.to_string()).await
        }
        (_, "/") => {
            write_response(stream, 200, "text/plain", "byteai serve — see /health, /v1/models, /v1/chat/completions, /tools, /v1/tools/<name>").await
        }
        _ => {
            write_response(stream, 404, "text/plain", "not found").await
        }
    }
}

async fn write_response(stream: &mut tokio::net::TcpStream, status: u16, content_type: &str, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}