//! `sessionsearch` — search across saved sessions (Hermes session_search
//! parity). Every saved session is a JSON file under `<data>/sessions/`.
//! This tool scans their message text, scores relevance with a lightweight
//! TF-IDF + cosine similarity (same pure-std approach as memsearch), and
//! returns the most relevant sessions with matching message excerpts.
//!
//! Without this, past sessions are write-only: the agent can list them but
//! cannot recall what was decided or done in an earlier conversation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use byteai_types::{SessionFile, ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct SessionSearchTool {
    data_dir: PathBuf,
}

impl SessionSearchTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

/// Load all session files (best-effort; skip unparseable).
fn load_all_sessions(dir: &PathBuf) -> Vec<SessionFile> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p)
            && let Ok(s) = serde_json::from_str::<SessionFile>(&text) {
                out.push(s);
            }
    }
    out
}

/// Combine all message content of a session into one searchable document.
fn session_doc(s: &SessionFile) -> String {
    let mut text = String::new();
    for m in &s.messages {
        if let Some(c) = &m.content {
            text.push_str(c);
            text.push('\n');
        }
    }
    text
}

/// Tokenize into lowercase alphanumeric tokens (drop short + stop words).
fn tokenize(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "that", "this", "your", "you", "are", "can", "how",
        "why", "when", "where", "which", "what", "have", "has", "was", "were", "will",
        "would", "could", "should", "from", "into", "about", "there", "their", "than",
        "then", "them", "they", "not", "but", "all", "any", "its", "our", "out", "was",
    ];
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch.to_ascii_lowercase());
        } else {
            if cur.len() >= 3 && !STOP.contains(&cur.as_str()) {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.len() >= 3 && !STOP.contains(&cur.as_str()) {
        out.push(cur);
    }
    out
}

/// Simple TF-IDF scores: query tokens scored against each doc by term
/// frequency weighted by inverse document frequency.
fn score_docs(query_tokens: &[String], docs: &[(String, String)]) -> Vec<(f64, usize)> {
    let n = docs.len().max(1) as f64;
    // Document frequency per token (owned keys, no leaks).
    let mut df: HashMap<String, usize> = HashMap::new();
    for (_, body) in docs {
        let mut local: HashMap<String, bool> = HashMap::new();
        for t in tokenize(body) {
            local.insert(t, true);
        }
        for k in local.keys() {
            *df.entry(k.clone()).or_insert(0) += 1;
        }
    }
    let mut scored = Vec::new();
    for (i, (_, body)) in docs.iter().enumerate() {
        let toks = tokenize(body);
        let mut tf: HashMap<String, usize> = HashMap::new();
        for t in &toks {
            *tf.entry(t.clone()).or_insert(0) += 1;
        }
        let mut score = 0.0;
        for q in query_tokens {
            if let Some(&count) = tf.get(q) {
                let idf = (n / (1.0 + *df.get(q).unwrap_or(&1) as f64)).ln() + 1.0;
                score += count as f64 * idf;
            }
        }
        scored.push((score, i));
    }
    scored
}

impl Tool for SessionSearchTool {
    fn name(&self) -> &'static str {
        "sessionsearch"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "sessionsearch".into(),
            description: "Search across all saved sessions (past conversations) by relevance. \
Returns the most relevant sessions with their id, model, timestamps, and matching message excerpts. \
Use when the user references earlier work, a decision from a past session, or asks 'did we already...'.\
Input: {query, limit?}."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "what to search for (keywords or natural language)"},
                    "limit": {"type": "integer", "description": "max sessions to return (default 5)"}
                },
                "required": ["query"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let data_dir = self.data_dir.clone();
        Box::pin(async move {
            let started = Instant::now();
            let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            let limit = args.get("limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(5).min(20);

            if query.trim().is_empty() {
                return ok_outcome("", "sessionsearch", "usage: sessionsearch <query> [limit]", 0);
            }

            let dir = data_dir.join("sessions");
            if !dir.is_dir() {
                return ok_outcome("", "sessionsearch", "no sessions directory yet", 0);
            }

            let q_tokens = tokenize(&query);
            if q_tokens.is_empty() {
                return ok_outcome("", "sessionsearch", "query has no searchable terms", 0);
            }

            // Blocking IO — run on the blocking pool.
            let handle = tokio::task::spawn_blocking(move || {
                let sessions = load_all_sessions(&dir);
                let docs: Vec<(String, String)> = sessions
                    .iter()
                    .map(|s| (s.id.clone(), session_doc(s)))
                    .collect();
                let mut scored = score_docs(&q_tokens, &docs);
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                scored.retain(|(score, _)| *score > 0.0);
                scored.truncate(limit);
                let mut out = String::new();
                if scored.is_empty() {
                    out.push_str("no sessions matched the query\n");
                }
                for (score, idx) in scored {
                    let s = &sessions[idx];
                    let doc = &docs[idx].1;
                    // Find the first matching excerpt (~160 chars around a hit).
                    let excerpt = {
                        let mut ex = String::from("(no excerpt)");
                        for t in &q_tokens {
                            if let Some(pos) = doc.to_lowercase().find(t) {
                                let start = pos.saturating_sub(60);
                                let end = (pos + 160).min(doc.len());
                                ex = doc[start..end].replace('\n', " ");
                                break;
                            }
                        }
                        ex
                    };
                    out.push_str(&format!(
                        "score={:.2} id={} model={} updated={}\n  …{excerpt}…\n",
                        score, s.id, s.model, s.updated_at
                    ));
                }
                out
            });

            let out = match handle.await {
                Ok(o) => o,
                Err(e) => format!("ERROR: {e}"),
            };
            ok_outcome("", "sessionsearch", out, started.elapsed().as_millis() as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_sessions(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_sess_test_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d.join("sessions")).unwrap();
        d
    }

    fn write_session(dir: &PathBuf, id: &str, messages: &[(&str, &str)]) {
        let msgs: Vec<byteai_types::Message> = messages
            .iter()
            .map(|(role, content)| match *role {
                "user" => byteai_types::Message::user(content.to_string()),
                "assistant" => byteai_types::Message::assistant(Some(content.to_string()), None, None),
                _ => byteai_types::Message::system(content.to_string()),
            })
            .collect();
        let sf = SessionFile {
            id: id.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-02T00:00:00Z".into(),
            model: "test".into(),
            provider: "test".into(),
            messages: msgs,
            usage: byteai_types::Usage::default(),
        };
        std::fs::write(
            dir.join("sessions").join(format!("{id}.json")),
            serde_json::to_string_pretty(&sf).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn finds_relevant_session() {
        let d = tmp_sessions("find");
        write_session(&d, "s1", &[
            ("user", "How do I configure the provider pool?"),
            ("assistant", "Set the providers list in config.toml with base_url and api_key."),
        ]);
        write_session(&d, "s2", &[
            ("user", "What color is the sky?"),
            ("assistant", "Blue on a clear day."),
        ]);
        let tool = SessionSearchTool::new(d.clone());
        let out = tool.execute(json!({"query":"provider pool config"})).await.output;
        assert!(out.contains("s1"), "expected s1 in results: {out}");
        assert!(!out.contains("s2"), "s2 should not match: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn empty_query_returns_usage() {
        let d = tmp_sessions("empty");
        let tool = SessionSearchTool::new(d.clone());
        let out = tool.execute(json!({"query":""})).await.output;
        assert!(out.contains("usage:"), "{out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn missing_dir_returns_graceful_message() {
        let d = tmp_sessions("nodir");
        std::fs::remove_dir_all(&d.join("sessions")).unwrap();
        let tool = SessionSearchTool::new(d.clone());
        let out = tool.execute(json!({"query":"anything"})).await.output;
        assert!(out.contains("no sessions"), "{out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn limit_caps_results() {
        let d = tmp_sessions("limit");
        for i in 0..6 {
            write_session(&d, &format!("s{i}"), &[
                ("user", &format!("task number {i} about refactoring")),
                ("assistant", "refactor done"),
            ]);
        }
        let tool = SessionSearchTool::new(d.clone());
        let out = tool.execute(json!({"query":"refactor","limit":3})).await.output;
        let hits = out.matches("score=").count();
        assert_eq!(hits, 3, "limit should cap to 3: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
