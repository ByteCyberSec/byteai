//! `autoskill` — self-evolving agent (unique to ByteAI; nothing like it in
//! deepseek-harness).
//!
//! ByteAI records *lessons* from successful work (pattern + context + what
//! worked). When the same lesson recurs, it is auto-promoted into a real,
//! loadable SKILL.md in `<data>/skills/` — the agent literally gets smarter
//! with use. This is a closed learning loop inside a single compiled binary:
//! no external service, no plugin framework, no model fine-tuning needed.
//!
//! Storage:
//!   lessons  -> <data>/learned/lessons.jsonl   (append-only, one per line)
//!   promoted -> <data>/skills/<name>/SKILL.md  (standard skill, loadable
//!              via the `skills` tool and injected like any other skill)

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

/// Promotion threshold: a lesson must recur this many times to become a skill.
const PROMOTE_AFTER: usize = 2;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Lesson {
    pub id: u64,
    pub pattern: String,
    pub context: String,
    pub ts: String,
    pub count: usize,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub struct AutoSkillTool {
    lessons_path: PathBuf,
    skills_root: PathBuf,
}

impl AutoSkillTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let learned = data_dir.join("learned");
        let _ = std::fs::create_dir_all(&learned);
        let skills_root = data_dir.join("skills");
        let _ = std::fs::create_dir_all(&skills_root);
        Self {
            lessons_path: learned.join("lessons.jsonl"),
            skills_root,
        }
    }

    fn load_lessons(&self) -> Vec<Lesson> {
        std::fs::read_to_string(&self.lessons_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Lesson>(l).ok())
            .collect()
    }

    fn append_lesson(&self, l: &Lesson) -> bool {
        if let Some(p) = self.lessons_path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        use std::io::Write;
        let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(&self.lessons_path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        writeln!(f, "{}", serde_json::to_string(l).unwrap_or_default()).is_ok()
    }

    /// Find the lesson with the same normalized pattern, if any.
    fn find(&self, pattern: &str, lessons: &[Lesson]) -> Option<Lesson> {
        let norm = pattern.trim().to_lowercase();
        lessons
            .iter()
            .find(|l| l.pattern.trim().to_lowercase() == norm)
            .cloned()
    }

    /// Promote a lesson into a real SKILL.md (standard format, loadable).
    fn promote(&self, l: &Lesson) -> Result<PathBuf, String> {
        let name: String = l
            .pattern
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .trim_matches('-')
            .to_string();
        let name = if name.is_empty() { format!("lesson-{}", l.id) } else { name };
        let dir = self.skills_root.join(&name);
        let _ = std::fs::create_dir_all(&dir);
        let body = format!(
            "---\nname: \"{name}\"\ndescription: \"Learned by ByteAI from repeated success: {}\"\nversion: 1.0.0\nauthor: ByteAI (AutoSkill)\ntags: [learned, autoskill]\n---\n\n# {name}\n\n## When to use\n{}\n\n## Pattern that worked\n{}\n",
            l.pattern.replace('"', "'"),
            l.context,
            l.pattern
        );
        let path = dir.join("SKILL.md");
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

impl Tool for AutoSkillTool {
    fn name(&self) -> &'static str {
        "autoskill"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "autoskill".into(),
            description: "Self-evolving agent: record lessons from successful work and auto-promote \
recurring patterns into real skills. Actions: learn <pattern> <context> (record what worked), \
list (show lessons + promotion candidates), promote <pattern> (force-promote a lesson to a \
SKILL.md now), forget <pattern>. When a lesson recurs twice it is promoted automatically — \
ByteAI gets smarter with use."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["learn", "list", "promote", "forget"], "description": "What to do." },
                    "pattern": { "type": "string", "description": "The reusable pattern that worked." },
                    "context": { "type": "string", "description": "When/where it applies (for learn)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let lessons_path = self.lessons_path.clone();
        let skills_root = self.skills_root.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list").to_string();
            let pattern = args.get("pattern").and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let context = args.get("context").and_then(|a| a.as_str()).unwrap_or("").trim().to_string();
            let elapsed = started.elapsed().as_millis() as u64;

            let t = AutoSkillTool {
                lessons_path: lessons_path.clone(),
                skills_root: skills_root.clone(),
            };

            match action.as_str() {
                "learn" => {
                    if pattern.is_empty() {
                        return ok_outcome("", "autoskill", "usage: autoskill learn <pattern> [context]".to_string(), elapsed);
                    }
                    let mut lessons = t.load_lessons();
                    if let Some(mut existing) = t.find(&pattern, &lessons) {
                        existing.count += 1;
                        // update in place: rewrite the file with the bumped count
                        let mut fresh = lessons
                            .iter()
                            .map(|l| {
                                if l.pattern.trim().to_lowercase() == pattern.trim().to_lowercase() {
                                    existing.clone()
                                } else {
                                    l.clone()
                                }
                            })
                            .collect::<Vec<_>>();
                        std::mem::swap(&mut fresh, &mut lessons);
                        let _ = std::fs::remove_file(&t.lessons_path);
                        for l in &lessons {
                            t.append_lesson(l);
                        }
                        if existing.count >= PROMOTE_AFTER {
                            match t.promote(&existing) {
                                Ok(path) => ok_outcome(
                                    "",
                                    "autoskill",
                                    format!(
                                        "lesson recurred {}x — auto-promoted to skill at {}\n(pattern: {})",
                                        existing.count,
                                        path.display(),
                                        existing.pattern
                                    ),
                                    elapsed,
                                ),
                                Err(e) => ok_outcome("", "autoskill", format!("promotion failed: {e}"), elapsed),
                            }
                        } else {
                            ok_outcome(
                                "",
                                "autoskill",
                                format!("lesson reinforced ({}x) — {} more occurrence(s) until it becomes a skill", existing.count, PROMOTE_AFTER - existing.count),
                                elapsed,
                            )
                        }
                    } else {
                        let lesson = Lesson {
                            id: now_ms(),
                            pattern: pattern.clone(),
                            context: context.clone(),
                            ts: now_iso(),
                            count: 1,
                        };
                        if t.append_lesson(&lesson) {
                            ok_outcome(
                                "",
                                "autoskill",
                                format!("lesson recorded (1x) — repeat this pattern and ByteAI will auto-promote it to a skill after {PROMOTE_AFTER} uses"),
                                elapsed,
                            )
                        } else {
                            ok_outcome("", "autoskill", "could not write lesson".to_string(), elapsed)
                        }
                    }
                }
                "list" => {
                    let lessons = t.load_lessons();
                    if lessons.is_empty() {
                        return ok_outcome("", "autoskill", "no lessons yet — record one: autoskill learn <pattern> [context]".to_string(), elapsed);
                    }
                    let mut out = format!("{} learned lesson(s):\n", lessons.len());
                    for l in &lessons {
                        let badge = if l.count >= PROMOTE_AFTER { "[SKILL ✓]" } else { "[...]" };
                        out.push_str(&format!("  {badge} {pattern} (x{count}) — {context}\n", pattern = l.pattern, count = l.count, context = l.context));
                    }
                    ok_outcome("", "autoskill", out, elapsed)
                }
                "promote" => {
                    if pattern.is_empty() {
                        return ok_outcome("", "autoskill", "usage: autoskill promote <pattern>".to_string(), elapsed);
                    }
                    match t.find(&pattern, &t.load_lessons()) {
                        Some(l) => match t.promote(&l) {
                            Ok(path) => ok_outcome("", "autoskill", format!("promoted lesson to skill at {}", path.display()), elapsed),
                            Err(e) => ok_outcome("", "autoskill", format!("promotion failed: {e}"), elapsed),
                        },
                        None => ok_outcome("", "autoskill", format!("no lesson matching {pattern:?} — learn it first"), elapsed),
                    }
                }
                "forget" => {
                    if pattern.is_empty() {
                        return ok_outcome("", "autoskill", "usage: autoskill forget <pattern>".to_string(), elapsed);
                    }
                    let lessons = t.load_lessons();
                    let kept: Vec<&Lesson> = lessons.iter().filter(|l| l.pattern.trim().to_lowercase() != pattern.trim().to_lowercase()).collect();
                    let _ = std::fs::remove_file(&t.lessons_path);
                    for l in &kept {
                        t.append_lesson(l);
                    }
                    ok_outcome("", "autoskill", format!("forgot lesson {pattern:?} ({} lessons remain)", kept.len()), elapsed)
                }
                other => ok_outcome("", "autoskill", format!("unknown action {other:?} — use learn | list | promote | forget"), elapsed),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_autoskill_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn learn_reinforce_promotes_to_skill() {
        let d = tmp_dir("promote");
        let t = AutoSkillTool::new(d.clone());

        // first learn
        let out = t
            .execute(json!({"action": "learn", "pattern": "when cargo test fails on borrow, run cargo check first", "context": "rust"}))
            .await;
        assert!(out.ok);
        assert!(out.output.contains("lesson recorded (1x)"));

        // reinforce -> auto-promote
        let out = t
            .execute(json!({"action": "learn", "pattern": "when cargo test fails on borrow, run cargo check first", "context": "rust"}))
            .await;
        assert!(out.ok);
        assert!(out.output.contains("auto-promoted"), "reinforced lesson promotes: {}", out.output);
        assert!(out.output.contains("SKILL.md"));

        // the promoted skill exists and is loadable by the skills tool's format
        let skill_dir = d.join("skills");
        let mut found = Vec::new();
        crate::skills::scan_dir(&skill_dir, &mut found, 0);
        assert_eq!(found.len(), 1, "one promoted skill");
        assert_eq!(found[0].name, "when-cargo-test-fails-on-borrow-run-cargo-check-first");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn list_and_forget() {
        let d = tmp_dir("list");
        let t = AutoSkillTool::new(d.clone());

        t.execute(json!({"action": "learn", "pattern": "cache provider list to avoid 401s", "context": "omniroute"})).await;
        let out = t.execute(json!({"action": "list"})).await;
        assert!(out.output.contains("1 learned lesson"), "list shows lesson: {}", out.output);

        let out = t.execute(json!({"action": "forget", "pattern": "cache provider list to avoid 401s"})).await;
        assert!(out.output.contains("0 lessons remain"));

        let _ = std::fs::remove_dir_all(&d);
    }
}
