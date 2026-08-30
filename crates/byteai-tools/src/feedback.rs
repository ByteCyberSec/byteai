//! `feedback` — recorded human feedback (DeepSeek-harness `feedback/` idea).
//!
//! Users submit a free-text remark about the session, and rate individual
//! assistant messages (thumbs / 1-5 stars / note). Feedback is a SIGNAL ABOUT
//! the output, never INPUT to the model: it is never injected into the prompt
//! or context. It's stored so humans and tooling can review how the agent did,
//! and (optionally) used by a separate review/improve pipeline.
//!
//! Storage: `<data>/feedback/remarks.json` (session remarks) and
//! `<data>/feedback/ratings.json` (per-message ratings). Append-only JSONL
//! lines, one record per line.

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn append_line(path: &PathBuf, line: &str) -> bool {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    writeln!(f, "{line}").is_ok()
}

pub struct FeedbackTool {
    remarks_path: PathBuf,
    ratings_path: PathBuf,
}

impl FeedbackTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("feedback");
        Self {
            remarks_path: dir.join("remarks.jsonl"),
            ratings_path: dir.join("ratings.jsonl"),
        }
    }
}

impl Tool for FeedbackTool {
    fn name(&self) -> &'static str {
        "feedback"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "feedback".into(),
            description: "Record human feedback: remark <text> (a free-text note about this session) or rate <msg_id> <1-5> [note]. \
                Feedback is never fed to the model — it is stored as a signal for humans to review the agent's work."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["remark", "rate", "stats"], "description": "What to do." },
                    "text": { "type": "string", "description": "Remark text (for remark) or note (for rate)." },
                    "msg_id": { "type": "string", "description": "Message id to rate (for rate)." },
                    "score": { "type": "integer", "description": "Rating 1-5 (for rate)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("stats").to_string();
            let text = args.get("text").and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let msg_id = args.get("msg_id").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let score = args.get("score").and_then(|a| a.as_u64()).unwrap_or(0);
            let elapsed = started.elapsed().as_millis() as u64;

            match action.as_str() {
                "remark" => {
                    if text.is_empty() {
                        return ok_outcome("", self.name(), "usage: feedback remark <text>".to_string(), elapsed);
                    }
                    let rec = json!({"ts": now_iso(), "ms": now_ms(), "remark": text});
                    if append_line(&self.remarks_path, &rec.to_string()) {
                        ok_outcome("", self.name(), format!("feedback recorded: {text}"), elapsed)
                    } else {
                        ok_outcome("", self.name(), format!("could not write feedback (path {})", self.remarks_path.display()), elapsed)
                    }
                }
                "rate" => {
                    if msg_id.is_empty() || score < 1 || score > 5 {
                        return ok_outcome("", self.name(), "usage: feedback rate <msg_id> <1-5> [note]".to_string(), elapsed);
                    }
                    let rec = json!({"ts": now_iso(), "ms": now_ms(), "msg_id": msg_id, "score": score, "note": text});
                    if append_line(&self.ratings_path, &rec.to_string()) {
                        ok_outcome("", self.name(), format!("rated {msg_id}: {score}/5"), elapsed)
                    } else {
                        ok_outcome("", self.name(), "could not write feedback".to_string(), elapsed)
                    }
                }
                "stats" => {
                    let remarks = std::fs::read_to_string(&self.remarks_path).unwrap_or_default();
                    let ratings = std::fs::read_to_string(&self.ratings_path).unwrap_or_default();
                    let r_count = remarks.lines().filter(|l| !l.trim().is_empty()).count();
                    let r_sum: i64 = ratings
                        .lines()
                        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                        .filter_map(|v| v.get("score").and_then(|s| s.as_i64()))
                        .sum();
                    let r_count_r = ratings.lines().filter(|l| !l.trim().is_empty()).count();
                    let avg = if r_count_r > 0 { r_sum as f64 / r_count_r as f64 } else { 0.0 };
                    ok_outcome(
                        "",
                        self.name(),
                        format!(
                            "feedback stats — remarks: {r_count}, ratings: {r_count_r} (avg {avg:.1}/5)\n\
                             stored under: {}",
                            self.remarks_path.parent().unwrap().display()
                        ),
                        elapsed,
                    )
                }
                other => ok_outcome("", self.name(), format!("unknown action {other:?} — use remark | rate | stats"), elapsed),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_fb_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn remark_rate_stats_roundtrip() {
        let d = tmp_dir("roundtrip");
        let t = FeedbackTool::new(d.clone());

        let out = t.execute(json!({"action": "remark", "text": "loved the speed"})).await;
        assert!(out.ok);
        assert!(out.output.contains("feedback recorded"));

        let out = t.execute(json!({"action": "rate", "msg_id": "m1", "score": 5, "text": "great answer"})).await;
        assert!(out.ok);
        assert!(out.output.contains("rated m1: 5/5"));

        // rate validation: bad score -> usage
        let out = t.execute(json!({"action": "rate", "msg_id": "m2", "score": 9})).await;
        assert!(out.output.contains("usage"));

        let out = t.execute(json!({"action": "stats"})).await;
        assert!(out.output.contains("remarks: 1"), "one remark: {}", out.output);
        assert!(out.output.contains("ratings: 1 (avg 5.0/5)"), "one 5-star rating: {}", out.output);

        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn empty_remark_reports_usage() {
        let d = tmp_dir("empty");
        let t = FeedbackTool::new(d.clone());
        let out = t.execute(json!({"action": "remark"})).await;
        assert!(out.output.contains("usage"), "empty remark -> usage: {}", out.output);
        let _ = std::fs::remove_dir_all(&d);
    }
}
