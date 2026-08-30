//! Web search tool (DuckDuckGo Lite — no API key, pure HTTP).
//!
//! ByteAi's answer lists "I can't browse the web interactively (but I can
//! fetch URLs)" as a limit. The most-starred browser agent (browser-use,
//! ~107k stars) solves this by giving the model a search+navigate loop, but
//! it is Python-only. This tool brings the same *capability* into the pure
//! Rust core with zero new runtime deps: it queries DuckDuckGo's Lite HTML
//! endpoint (same public endpoint a browser uses, no key, no account) and
//! returns clean title/URL/snippet results. Combined with the existing
//! `fetch` tool (read the URL body), the agent now has an interactive
//! browse-the-web loop: search -> pick a URL -> fetch -> refine.

use std::time::Duration;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BYTES: usize = 512_000;

pub struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "websearch"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "websearch".into(),
            description: "Search the web and return a list of results (title, URL, snippet). \
No API key required (DuckDuckGo Lite endpoint, same as a browser). \
Use to discover URLs, then pass the best URL to the fetch tool to read the full page. \
For the latest info: websearch the query, then fetch the most relevant result.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query (keywords, ~2-6 words works best)" },
                    "max": { "type": "integer", "description": "Max results to return (default 5, max 10)" }
                },
                "required": ["query"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("").to_string();
            if query.trim().is_empty() {
                return ok_outcome("", "websearch", "ERROR: `query` is required".to_string(), started.elapsed().as_millis() as u64);
            }
            let max = args.get("max").and_then(|m| m.as_u64()).unwrap_or(5).min(10) as usize;

            let client = match reqwest::Client::builder()
                .timeout(SEARCH_TIMEOUT)
                .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ByteAi/1.0 websearch")
                .build()
            {
                Ok(c) => c,
                Err(e) => return ok_outcome("", "websearch", format!("client build failed: {e}"), started.elapsed().as_millis() as u64),
            };

            let resp = client
                .post("https://lite.duckduckgo.com/lite/")
                .form(&[("q", query.trim())])
                .send()
                .await;
            let body = match resp {
                Ok(r) if r.status().is_success() => r.bytes().await.unwrap_or_default(),
                Ok(r) => return ok_outcome("", "websearch", format!("HTTP {}", r.status()), started.elapsed().as_millis() as u64),
                Err(e) => return ok_outcome("", "websearch", format!("request failed: {e}"), started.elapsed().as_millis() as u64),
            };

            let html = String::from_utf8_lossy(&body[..body.len().min(MAX_BYTES)]);
            let results = parse_lite_results(&html);
            if results.is_empty() {
                let out = format!(
                    "# websearch: {query}\n\nNo results returned. Try fewer/broader keywords, or fetch a likely URL directly.",
                );
                return ok_outcome("", "websearch", out, started.elapsed().as_millis() as u64);
            }

            let mut out = format!("# websearch: {query}\n\n");
            for (i, r) in results.iter().take(max).enumerate() {
                out.push_str(&format!("{}. {}\n   {}\n   {}\n\n", i + 1, r.title, r.url, r.snippet));
            }
            out.push_str("To read a page, call fetch with its URL.");
            ok_outcome("", "websearch", out, started.elapsed().as_millis() as u64)
        })
    }
}

struct LiteResult {
    title: String,
    url: String,
    snippet: String,
}

/// Parse DuckDuckGo Lite HTML: result rows are `<a rel="nofollow"
/// href="URL" class="result-link">Title</a>` (single or double quoted
/// attributes), followed by a snippet cell `<td class="result-snippet">…</td>`.
/// We scan for those markers in order and pair each link with the next snippet.
/// Advancement is precise (past `</a>` / `</td>`) so marker-like text inside
/// a snippet can never derail the state machine.
fn parse_lite_results(html: &str) -> Vec<LiteResult> {
    let mut results = Vec::new();
    let mut rest = html;

    // State for the current result being assembled.
    let mut pending_title: Option<String> = None;
    let mut pending_url: Option<String> = None;

    loop {
        // Find the next marker of each kind; process whichever comes first.
        // Accept both single-quote and double-quote class attributes.
        let link_at = rest.find("result-link");
        let snip_at = rest.find("result-snippet");
        let (is_link, at) = match (link_at, snip_at) {
            (Some(l), Some(s)) if l <= s => (true, l),
            (Some(_), Some(s)) => (false, s), // snippet comes first
            (Some(l), None) => (true, l),
            (None, Some(s)) => (false, s),
            (None, None) => break,
        };

        // Find the class attribute (single or double quoted) containing the marker.
        let quote_char = rest[..at].rfind(['"', '\'']).map(|p| rest.as_bytes()[p] as char);
        let class_start = match quote_char {
            Some(_) => rest[..at].rfind(['"', '\'']).unwrap_or(0),
            None => at.saturating_sub(1),
        };
        let qc = rest.as_bytes()[class_start] as char; // " or '
        let after_attr = rest[at..].find(qc).map(|e| at + e + 1).unwrap_or(rest.len());

        if is_link {
            let tag_start = rest[..class_start].rfind("<a").unwrap_or(0);
            let tag = &rest[tag_start..after_attr];
            let url = extract_attr(tag, "href");
            // Content after the class attribute's closing quote.
            let content = &rest[after_attr..];
            let title = match content.find('>') {
                Some(gt) => {
                    let inner = &content[gt + 1..];
                    match inner.find("</a>") {
                        Some(end) => strip_inner(inner[..end].to_string()),
                        None => String::new(),
                    }
                }
                None => String::new(),
            };
            pending_title = Some(title);
            pending_url = url;
            // Advance past the anchor close: content starts at after_attr + gt + 1,
            // and the title ends before `</a>`.
            match content.find('>') {
                Some(gt) => match content[gt + 1..].find("</a>") {
                    Some(end) => {
                        rest = &rest[after_attr + gt + 1 + end + "</a>".len()..];
                    }
                    None => break,
                },
                None => break,
            }
        } else {
            let content = &rest[after_attr..];
            let snippet = match content.find('>') {
                Some(gt) => {
                    let inner = &content[gt + 1..];
                    match inner.find("</td>") {
                        Some(end) => strip_inner(inner[..end].to_string()),
                        None => String::new(),
                    }
                }
                None => String::new(),
            };
            if let (Some(t), Some(u)) = (pending_title.take(), pending_url.take())
                && !t.is_empty() && !u.is_empty() {
                    results.push(LiteResult { title: t, url: u, snippet });
                }
            // Advance past the snippet cell close.
            match content.find('>') {
                Some(gt) => match content[gt + 1..].find("</td>") {
                    Some(end) => {
                        rest = &rest[after_attr + gt + 1 + end + "</td>".len()..];
                    }
                    None => break,
                },
                None => break,
            }
        }
    }
    results
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    // Try double-quoted first
    let pat1 = format!("{name}=\"");
    if let Some(i) = tag.find(&pat1) {
        let rest = &tag[i + pat1.len()..];
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    // Try single-quoted
    let pat2 = format!("{name}='");
    let i = tag.find(&pat2)? + pat2.len();
    let rest = &tag[i..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Strip a bit of HTML from title/snippet text and collapse whitespace.
fn strip_inner(raw: String) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        if c == '<' {
            in_tag = true;
            continue;
        }
        if c == '>' {
            in_tag = false;
            continue;
        }
        if !in_tag {
            out.push(c);
        }
    }
    // Collapse runs of whitespace (DDG snippets include newlines/spans).
    let words: Vec<&str> = out.split_whitespace().collect();
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_lite_result() {
        let html = r#"<html><body>
            <a rel="nofollow" href="https://example.com/page" class="result-link">Example <b>Page</b></a>
            <td class="result-snippet">This is a snippet about example dot com</td>
        </body></html>"#;
        let r = parse_lite_results(html);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Example Page");
        assert_eq!(r[0].url, "https://example.com/page");
        assert_eq!(r[0].snippet, "This is a snippet about example dot com");
    }

    #[test]
    fn parses_multiple_results_in_order() {
        let html = r#"<html>
            <a rel="nofollow" href="https://a.com" class="result-link">First</a>
            <td class="result-snippet">snippet one</td>
            <a rel="nofollow" href="https://b.com" class="result-link">Second</a>
            <td class="result-snippet">snippet two</td>
        </html>"#;
        let r = parse_lite_results(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://a.com");
        assert_eq!(r[1].title, "Second");
        assert_eq!(r[1].snippet, "snippet two");
    }

    #[test]
    fn empty_html_gives_no_results() {
        assert!(parse_lite_results("").is_empty());
        assert!(parse_lite_results("<html><body><p>nothing here</p></body></html>").is_empty());
    }

    #[test]
    fn strips_inner_tags_from_snippet() {
        assert_eq!(strip_inner("a <span>b</span> c".into()), "a b c");
        assert_eq!(strip_inner("no tags".into()), "no tags");
    }

    #[test]
    fn parses_single_quoted_attributes() {
        // DuckDuckGo Lite uses single quotes for class attributes
        let html = r#"<html>
            <a rel="nofollow" href='https://tokio.rs/tokio/tutorial/async' class='result-link'>Async in depth | Tokio</a>
            <td class='result-snippet'>Tokio is a runtime for async Rust.</td>
        </html>"#;
        let r = parse_lite_results(html);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title, "Async in depth | Tokio");
        assert_eq!(r[0].url, "https://tokio.rs/tokio/tutorial/async");
        assert_eq!(r[0].snippet, "Tokio is a runtime for async Rust.");
    }

    #[test]
    fn snippet_text_containing_marker_names_does_not_break() {
        // Snippet text contains "result-link" and "result-snippet" deliberately
        let html = r#"<html>
            <a rel="nofollow" href="https://a.com" class="result-link">First</a>
            <td class="result-snippet">This text contains result-link and result-snippet words</td>
            <a rel="nofollow" href="https://b.com" class="result-link">Second</a>
            <td class="result-snippet">second snippet</td>
        </html>"#;
        let r = parse_lite_results(html);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].url, "https://a.com");
        assert_eq!(r[0].snippet, "This text contains result-link and result-snippet words");
        assert_eq!(r[1].title, "Second");
        assert_eq!(r[1].snippet, "second snippet");
    }
}
