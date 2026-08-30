//! Fetch tool (firecrawl-inspired). Fetch a URL and return clean text.
//! Uses reqwest (already a dep) to GET the URL, strips HTML tags, converts
//! common entities, and returns the plain text. 10-second timeout, 256KB cap.

use std::time::Duration;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BYTES: usize = 256_000;

pub struct FetchTool;

impl Tool for FetchTool {
    fn name(&self) -> &'static str {
        "fetch"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "fetch".into(),
            description: "Fetch a URL and return the page content as clean text. \
Strips HTML tags, converts entities, capped at 256KB. 10-second timeout. \
Use for reading documentation, checking API responses, or scraping text from web pages. \
Based on the firecrawl pattern (1st-party web extraction).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "HTTP or HTTPS URL to fetch" },
                    "max_chars": { "type": "integer", "description": "Max characters to return (default 8000)" }
                },
                "required": ["url"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let url = args.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
            if url.is_empty() || !url.starts_with("http") {
                return ok_outcome("", "fetch", "ERROR: `url` must be http(s)://...".to_string(), started.elapsed().as_millis() as u64);
            }
            let max_chars = args.get("max_chars").and_then(|m| m.as_u64()).unwrap_or(8000).min(100_000) as usize;

            let client = match reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .user_agent("ByteAi/1.0 (fetch tool)")
                .danger_accept_invalid_certs(false)
                .build()
            {
                Ok(c) => c,
                Err(e) => return ok_outcome("", "fetch", format!("client build failed: {e}"), started.elapsed().as_millis() as u64),
            };

            let resp = client.get(&url).send().await;
            let body = match resp {
                Ok(r) if r.status().is_success() => r.bytes().await.unwrap_or_default(),
                Ok(r) => {
                    return ok_outcome("", "fetch", format!("HTTP {}", r.status()), started.elapsed().as_millis() as u64);
                }
                Err(e) => {
                    return ok_outcome("", "fetch", format!("request failed: {e}"), started.elapsed().as_millis() as u64);
                }
            };

            let text = String::from_utf8_lossy(&body[..body.len().min(MAX_BYTES)]);
            let clean = strip_html(&text);
            let mut out = format!("# Fetched: {url}\n\n");
            let preview: String = clean.chars().take(max_chars).collect();
            out.push_str(&preview);
            if clean.len() > max_chars {
                out.push_str(&format!("\n\n[... truncated at {max_chars} chars; raw was {} bytes, clean was {} chars]", body.len(), clean.len()));
            }
            ok_outcome("", "fetch", out, started.elapsed().as_millis() as u64)
        })
    }
}

/// Strip HTML tags, unescape common entities, normalize whitespace.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_entity = false;
    let mut skip_block = false; // inside <style> or <script>
    let mut tag_buf = String::new();
    let mut entity_buf = String::new();
    let mut prev_was_newline = false;

    for c in html.chars() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                let tag = tag_buf.to_lowercase();
                // Enter skip mode for style/script blocks (no output until close).
                if matches!(tag.as_str(), "style" | "script" | "noscript") {
                    skip_block = true;
                } else if tag.starts_with('/') && matches!(tag[1..].as_ref(), "style" | "script" | "noscript") {
                    skip_block = false;
                }
                tag_buf.clear();
            } else {
                tag_buf.push(c);
            }
            continue;
        }
        if skip_block {
            // Still need to track closing tag start.
            if c == '<' {
                in_tag = true;
            }
            continue;
        }
        if c == '<' {
            in_tag = true;
            tag_buf.clear();
            continue;
        }
        if c == '&' {
            in_entity = true;
            entity_buf.clear();
            continue;
        }
        if in_entity {
            if c == ';' {
                let decoded = match entity_buf.as_str() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    "nbsp" => " ",
                    _ => "", // drop unknown entities
                };
                out.push_str(decoded);
                in_entity = false;
                continue;
            }
            entity_buf.push(c);
            continue;
        }
        // Normalize whitespace: collapse multiple newlines to one.
        if c == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else {
            prev_was_newline = false;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_basic() {
        let html = "<p>Hello <b>world</b> &amp; everyone</p>";
        assert_eq!(strip_html(html), "Hello world & everyone");
    }

    #[test]
    fn strip_html_entities() {
        let html = "a &lt; b &gt; c &quot;d&quot; &apos;e&apos;";
        assert_eq!(strip_html(html), "a < b > c \"d\" 'e'");
    }

    #[test]
    fn strip_style_script() {
        let html = "<div>visible</div><style>body{color:red}</style><script>alert(1)</script><p>ok</p>";
        assert_eq!(strip_html(html), "visibleok");
    }
}