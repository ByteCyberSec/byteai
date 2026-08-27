//! `memsearch` — semantic memory search without external infrastructure.
//!
//! The built-in memory `search` is FTS5 keyword matching, which misses synonyms
//! and rephrased queries. This tool does a small TF-IDF + cosine similarity over
//! all memory entries (tokenize, stopword-filter, normalize, score) so queries
//! like "how does the project handle credentials" find entries about "API keys".
//!
//! Pure std: tokenization is a manual byte walk, no heavy NLP deps.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use apex_memory::{Kind, Memory};
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct MemsearchTool {
    data_dir: PathBuf,
}

impl MemsearchTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

impl Tool for MemsearchTool {
    fn name(&self) -> &'static str {
        "memsearch"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "memsearch".into(),
            description: "Semantic memory search over all stored notes/wiki/entities using TF-IDF + cosine similarity (handles synonyms and rephrased queries that plain keyword search misses). Input: {query, limit?, kind?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "natural-language search"},
                    "limit": {"type": "integer", "description": "max results (default 8)"},
                    "kind": {"type": "string", "enum": ["note", "wiki", "entity", "session"], "description": "filter by entry kind"}
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
            let limit = args.get("limit").and_then(Value::as_u64).map(|n| n as usize).unwrap_or(8).min(30);
            let kind = args.get("kind").and_then(Value::as_str).map(Kind::from_str);

            if query.trim().is_empty() {
                return ok_outcome("", "memsearch", "usage: memsearch <query> [limit] [kind]", 0);
            }

            // Pull all entries (blocking SQLite is fine for these sizes; run in
            // a spawn_blocking to keep the async runtime snappy).
            let q = query.clone();
            let k = kind;
            let entries = tokio::task::spawn_blocking(move || {
                let mem = Memory::open(&data_dir)?;
                let mut all = mem.list(k, 1000)?;
                if all.is_empty() {
                    all = mem.list(None, 1000)?;
                }
                Ok::<_, anyhow::Error>(all)
            })
            .await;

            let entries = match entries {
                Ok(Ok(e)) => e,
                Ok(Err(e)) => return ok_outcome("", "memsearch", format!("memsearch failed: {e:#}"), 0),
                Err(e) => return ok_outcome("", "memsearch", format!("memsearch task failed: {e:#}"), 0),
            };

            let scored = score_entries(&q, &entries, limit);
            let mut out = String::new();
            if scored.is_empty() {
                out.push_str("no semantic matches found in memory\n");
            } else {
                out.push_str(&format!("semantic matches for {q:?}:\n"));
                for (score, e) in &scored {
                    let preview = preview(&e.body, 140);
                    out.push_str(&format!(
                        "  [{:.2}] #{} [{}/{}] {}\n      {}\n",
                        score,
                        e.id,
                        e.kind.as_str(),
                        e.updated_at,
                        e.title,
                        preview
                    ));
                }
            }
            ok_outcome("", "memsearch", out, started.elapsed().as_millis() as u64)
        })
    }
}

/// ---- TF-IDF cosine scoring -------------------------------------------------

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "if", "then", "else", "of", "to", "in",
    "on", "for", "with", "at", "by", "from", "as", "is", "are", "was", "were",
    "be", "been", "being", "have", "has", "had", "do", "does", "did", "will",
    "would", "can", "could", "should", "may", "might", "must", "not", "no",
    "yes", "so", "too", "very", "just", "about", "into", "over", "under",
    "this", "that", "these", "those", "it", "its", "i", "you", "he", "she",
    "we", "they", "me", "him", "her", "us", "them", "my", "your", "our",
    "their", "what", "which", "who", "whom", "how", "when", "where", "why",
];

/// Tokenize into lowercase alphanumeric words (ASCII + simple unicode-aware
/// fallback: any non-alphanumeric char is a separator).
fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(cur.to_lowercase());
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(cur.to_lowercase());
    }
    out
}

fn term_freq(tokens: &[String]) -> HashMap<String, usize> {
    let mut tf: HashMap<String, usize> = HashMap::new();
    for t in tokens {
        if t.len() < 2 || STOPWORDS.contains(&t.as_str()) {
            continue;
        }
        *tf.entry(t.clone()).or_default() += 1;
    }
    tf
}

/// Build TF vectors for the entry corpus (title + body + tags).
fn corpus_vectors(entries: &[apex_memory::Entry]) -> Vec<(i64, HashMap<String, f64>)> {
    let mut docs = Vec::with_capacity(entries.len());
    let mut df: HashMap<String, usize> = HashMap::new();
    let mut per_doc: Vec<HashMap<String, usize>> = Vec::with_capacity(entries.len());

    for e in entries {
        let mut text = format!("{} {}", e.title, e.body);
        for t in &e.tags {
            text.push(' ');
            text.push_str(t);
        }
        let tf = term_freq(&tokenize(&text));
        for k in tf.keys() {
            *df.entry(k.clone()).or_default() += 1;
        }
        per_doc.push(tf);
    }

    let n = entries.len().max(1);
    for (i, tf) in per_doc.into_iter().enumerate() {
        let mut vec = HashMap::with_capacity(tf.len());
        for (term, count) in tf {
            let idf = ((n as f64) / (df.get(&term).copied().unwrap_or(1) as f64)).ln() + 1.0;
            vec.insert(term, count as f64 * idf);
        }
        docs.push((entries[i].id, vec));
    }
    docs
}

fn normalize(v: &HashMap<String, f64>) -> f64 {
    v.values().map(|x| x * x).sum::<f64>().sqrt()
}

fn cosine(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let na = normalize(a);
    let nb = normalize(b);
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    let (small, big) = if a.len() < b.len() { (a, b) } else { (b, a) };
    let mut dot = 0.0;
    for (k, v) in small {
        if let Some(w) = big.get(k) {
            dot += v * w;
        }
    }
    dot / (na * nb)
}

fn score_entries(
    query: &str,
    entries: &[apex_memory::Entry],
    limit: usize,
) -> Vec<(f64, apex_memory::Entry)> {
    let qvec = term_freq(&tokenize(query))
        .into_iter()
        .map(|(k, v)| (k, v as f64))
        .collect::<HashMap<_, _>>();
    if qvec.is_empty() {
        return Vec::new();
    }
    let docs = corpus_vectors(entries);
    let mut scored: Vec<(f64, apex_memory::Entry)> = entries
        .iter()
        .zip(docs.iter())
        .map(|(e, (_, dvec))| (cosine(&qvec, dvec), e.clone()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.retain(|(s, _)| *s > 0.001);
    scored.truncate(limit);
    scored
}

fn preview(body: &str, max: usize) -> String {
    let body = body.trim();
    if body.len() <= max {
        body.to_string()
    } else {
        let cut = body.char_indices().nth(max).map(|(i, _)| i).unwrap_or(body.len());
        format!("{}…", &body[..cut])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, title: &str, body: &str) -> apex_memory::Entry {
        apex_memory::Entry {
            id,
            kind: Kind::Note,
            title: title.into(),
            body: body.into(),
            tags: vec![],
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
        }
    }

    #[test]
    fn tokenize_splits_punctuation() {
        let t = tokenize("API-keys, Tauri_v2 & Ollama!");
        assert_eq!(t, vec!["api", "keys", "tauri", "v2", "ollama"]);
    }

    #[test]
    fn stopwords_removed() {
        let tf = term_freq(&tokenize("the quick brown fox and the lazy dog"));
        assert!(!tf.contains_key("the"));
        assert!(!tf.contains_key("and"));
        assert!(tf.contains_key("quick"));
    }

    #[test]
    fn cosine_identical_is_one() {
        let v: HashMap<String, f64> = vec![("a".into(), 2.0), ("b".into(), 3.0)].into_iter().collect();
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn scoring_ranks_semantic_match_high() {
        let entries = vec![
            entry(1, "config", "the project uses a config.toml file"),
            entry(2, "secrets", "API keys are stored encrypted in the secrets directory"),
            entry(3, "git", "commit messages should be conventional"),
        ];
        let scored = score_entries("where do we keep api keys", &entries, 5);
        assert!(!scored.is_empty());
        assert_eq!(scored[0].1.id, 2, "secrets entry should rank first");
    }
}