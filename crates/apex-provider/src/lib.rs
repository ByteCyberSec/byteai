//! OpenAI-compatible streaming chat client.
//!
//! One client speaks to every OpenAI-compatible endpoint (OmniRoute, Ollama,
//! LM Studio, vLLM, OpenRouter, hosted gateways). Streaming parses SSE lines
//! manually (no extra dependency) and accumulates tool-call deltas by index.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use apex_types::{Message, ToolDef, Usage};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct Client {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

/// Streaming events emitted by `chat_stream`.
#[derive(Debug)]
pub enum StreamEvent {
    /// Content delta (assistant text).
    Content(String),
    /// Reasoning delta (o-series `reasoning_content`).
    Reasoning(String),
    /// A tool-call delta: (index, id, name, arguments-delta).
    ToolCallDelta(usize, String, String, String),
    /// Usage received at stream end (may be absent on some endpoints).
    Usage(Usage),
    /// Stream finished.
    Done,
}

/// Accumulated result of a streaming turn.
#[derive(Debug, Default)]
pub struct TurnResult {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCallAccum>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolCallAccum {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Client {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .context("build http client")?;
        Ok(Self { base_url, api_key: api_key.into(), http })
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        if !self.api_key.is_empty() {
            h.insert(AUTHORIZATION, format!("Bearer {}", self.api_key).parse().unwrap());
        }
        h
    }

    /// GET /v1/models — returns model ids (for `doctor`).
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.base_url);
        let resp = self.http.get(&url).headers(self.headers()).send().await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("models endpoint returned {status}: {}", truncate(&body, 300));
        }
        let v: Value = serde_json::from_str(&body).context("parse models response")?;
        let ids = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(ids)
    }

    /// Non-streaming chat completion (used by tests and `doctor`).
    pub async fn chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: Option<u32>,
    ) -> Result<(String, Vec<ToolCallAccum>, Usage)> {
        let mut acc = TurnResult::default();
        let mut done = false;
        self.chat_stream(model, messages, tools, max_tokens, |ev| {
            match ev {
                StreamEvent::Content(c) => acc.content.push_str(&c),
                StreamEvent::Reasoning(r) => acc.reasoning.push_str(&r),
                StreamEvent::ToolCallDelta(i, id, name, args) => {
                    let pos = acc.tool_calls.iter().position(|t| t.index == i);
                    match pos {
                        Some(p) => {
                            if !id.is_empty() {
                                acc.tool_calls[p].id = id;
                            }
                            if !name.is_empty() {
                                acc.tool_calls[p].name = name;
                            }
                            acc.tool_calls[p].arguments.push_str(&args);
                        }
                        None => acc.tool_calls.push(ToolCallAccum { index: i, id, name, arguments: args }),
                    }
                }
                StreamEvent::Usage(u) => acc.usage = Some(u),
                StreamEvent::Done => done = true,
            }
        })
        .await?;
        let _ = done;
        let usage = acc.usage.clone().unwrap_or_default();
        Ok((acc.content, acc.tool_calls, usage))
    }

    /// Streaming chat completion. Calls `on_event` for each parsed event.
    ///
    /// Transient failures (HTTP 429 rate-limit, 5xx, connection drops, timeouts)
    /// are retried with exponential backoff + jitter (respecting `Retry-After`
    /// when the provider sends it). Without this, one rate-limit reply kills an
    /// otherwise healthy multi-iteration run at `Phase::Blocked`.
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: Option<u32>,
        mut on_event: impl FnMut(StreamEvent),
    ) -> Result<()> {
        let url = format!("{}/chat/completions", self.base_url);
        let wire: Vec<Value> = messages.iter().map(Message::to_wire).collect();
        let mut body = serde_json::json!({
            "model": model,
            "messages": wire,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !tools.is_empty() {
            let tools_wire: Vec<Value> = tools
                .iter()
                .map(|t| serde_json::json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
                }))
                .collect();
            body["tools"] = serde_json::json!(tools_wire);
        }
        if let Some(mt) = max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }

        // --- Send with retry on transient errors (429 / 5xx / network / timeout).
        // Events only flow after the status check passes, so a retry never
        // duplicates content already handed to on_event.
        const MAX_ATTEMPTS: u32 = 4;
        const BASE_DELAY_MS: u64 = 1_000;
        let mut attempt: u32 = 0;
        let resp = loop {
            attempt += 1;
            match self
                .http
                .post(&url)
                .headers(self.headers())
                .json(&body)
                .send()
                .await
                .with_context(|| format!("POST {url}"))
            {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        break resp;
                    }
                    if !is_transient(&status) || attempt >= MAX_ATTEMPTS {
                        let text = resp.text().await.unwrap_or_default();
                        bail!("chat completions returned {status}: {}", truncate(&text, 300));
                    }
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    let delay = retry_after.map(Duration::from_secs)
                        .unwrap_or_else(|| {
                            let jitter = jitter_ms();
                            Duration::from_millis(BASE_DELAY_MS.saturating_mul(1 << (attempt - 1)) + jitter)
                        });
                    tracing::warn!(
                        "chat_stream transient error {status} on attempt {attempt}/{MAX_ATTEMPTS}; retrying in {delay:?}"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(e) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(e);
                    }
                    let jitter = jitter_ms();
                    let delay = Duration::from_millis(BASE_DELAY_MS.saturating_mul(1 << (attempt - 1)) + jitter);
                    tracing::warn!("chat_stream send error (attempt {attempt}/{MAX_ATTEMPTS}): {e:#}; retrying in {delay:?}");
                    tokio::time::sleep(delay).await;
                }
            }
        };

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read stream chunk")?;
            buf.extend_from_slice(&chunk);
            // Split on newlines, keeping the trailing partial line in buf.
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        on_event(StreamEvent::Done);
                        return Ok(());
                    }
                    match parse_chunk(data) {
                        Ok(evs) => {
                            for ev in evs {
                                on_event(ev);
                            }
                        }
                        Err(e) => debug!("unparseable chunk: {e}: {data:?}"),
                    }
                }
            }
        }
        on_event(StreamEvent::Done);
        Ok(())
    }
}

/// Parse one SSE `data:` payload into zero or more stream events.
fn parse_chunk(data: &str) -> Result<Vec<StreamEvent>> {
    let v: Value = serde_json::from_str(data).context("parse SSE json")?;
    let mut evs = Vec::new();
    // Usage-only chunk (some providers send it at the end or in every chunk).
    if let Some(u) = v.get("usage")
        && !u.is_null() {
            let usage = parse_usage(u)?;
            evs.push(StreamEvent::Usage(usage));
            return Ok(evs);
        }
    let choice = match v.get("choices").and_then(|c| c.as_array()).and_then(|c| c.first()) {
        Some(c) => c,
        None => return Ok(evs),
    };
    let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
    if let Some(c) = delta.get("content").and_then(|c| c.as_str())
        && !c.is_empty() {
            evs.push(StreamEvent::Content(c.to_string()));
        }
    if let Some(r) = delta.get("reasoning_content").and_then(|c| c.as_str())
        && !r.is_empty() {
            evs.push(StreamEvent::Reasoning(r.to_string()));
        }
    if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tcs {
            let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
            let id = tc.get("id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let args = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() && name.is_empty() && args.is_empty() {
                continue;
            }
            evs.push(StreamEvent::ToolCallDelta(index, id, name, args));
        }
    }
    Ok(evs)
}

fn parse_usage(u: &Value) -> Result<Usage> {
    Ok(Usage {
        prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        completion_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        total_tokens: u.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect::<String>() + "…" }
}

/// Whether an HTTP status is worth retrying (rate-limit / server / gateway).
fn is_transient(status: &reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    code == 408 || code == 409 || code == 425 || code == 429
        || code == 500 || code == 502 || code == 503 || code == 504
}

/// Tiny deterministic jitter (0..500ms) from the clock — avoids thundering
/// herds without pulling in a rand dependency.
fn jitter_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % 500
}

#[allow(dead_code)]
fn _assert_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Client>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_statuses_are_retryable() {
        for code in [408u16, 409, 425, 429, 500, 502, 503, 504] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert!(is_transient(&status), "{code} should be transient");
        }
        for code in [400u16, 401, 403, 404, 422] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert!(!is_transient(&status), "{code} should NOT be transient");
        }
    }

    #[test]
    fn jitter_is_bounded() {
        for _ in 0..50 {
            assert!(jitter_ms() < 500);
        }
    }

    #[test]
    fn truncate_short_and_long() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }
}

pub mod router;
