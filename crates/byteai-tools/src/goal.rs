//! `goal` — one durable completion objective per session (DeepSeek-harness `goal/` idea).
//!
//! Gives a session a single durable goal that survives restarts, resume, and
//! fork: the agent can set/update it with a tool, the human can read or clear
//! it with `/goal`, and the current goal is injected into context so every
//! turn keeps working toward it. Only one goal is current at a time, and a
//! goal is *state*, not a scheduler — automatic continuation is the
//! `auto_continue` agent setting, which reads this goal as its anchor.
//!
//! Storage: `<data>/goals/<session_id>.json` (or `default.json` when no
//! session id is available). Nothing else keeps a separate store.

use std::path::PathBuf;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

/// A durable goal.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    pub session: String,
    pub text: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub completed: bool,
}

pub struct GoalTool {
    dir: PathBuf,
}

impl GoalTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("goals");
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn path(&self, session: &str) -> PathBuf {
        let slug: String = session
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let slug = if slug.is_empty() { "default".to_string() } else { slug };
        self.dir.join(format!("{slug}.json"))
    }

    pub fn load(&self, session: &str) -> Option<Goal> {
        std::fs::read_to_string(self.path(session))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .filter(|g: &Goal| !g.completed)
    }

    fn save(&self, g: &Goal) {
        let _ = std::fs::write(self.path(&g.session), serde_json::to_string_pretty(g).unwrap_or_default());
    }

    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

impl Tool for GoalTool {
    fn name(&self) -> &'static str {
        "goal"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "goal".into(),
            description: "One durable completion objective for this session, surviving restart/resume. Actions: \
                set <text> | get | clear | complete. The goal anchors auto-continue: keep working toward it every turn. \
                Only one goal is current at a time."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["set", "get", "clear", "complete"], "description": "What to do." },
                    "text": { "type": "string", "description": "Goal text (for set)." },
                    "session": { "type": "string", "description": "Optional session id (default: current session)." }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("get").to_string();
            let text = args.get("text").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let session = args.get("session").and_then(|a| a.as_str()).unwrap_or("default").to_string();
            let elapsed = started.elapsed().as_millis() as u64;

            match action.as_str() {
                "set" => {
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        return ok_outcome("", self.name(), "usage: goal set <text>".to_string(), elapsed);
                    }
                    let now = self.now_ms();
                    let g = match self.load(&session) {
                        Some(mut g) => {
                            g.text = text.clone();
                            g.updated_ms = now;
                            g
                        }
                        None => Goal {
                            session: session.clone(),
                            text: text.clone(),
                            created_ms: now,
                            updated_ms: now,
                            completed: false,
                        },
                    };
                    self.save(&g);
                    ok_outcome(
                        "",
                        self.name(),
                        format!("goal set for session {session}: {text}\nkeep working toward it every turn (auto-continue anchor)."),
                        elapsed,
                    )
                }
                "get" => {
                    match self.load(&session) {
                        Some(g) => ok_outcome(
                            "",
                            self.name(),
                            format!("current goal (session {session}):\n  {}\ncreated {} · updated {}\n\nKeep working toward it. Use `goal complete` when done.",
                                g.text, g.created_ms, g.updated_ms),
                            elapsed,
                        ),
                        None => ok_outcome(
                            "",
                            self.name(),
                            format!("no active goal for session {session} — set one with: goal set <text>"),
                            elapsed,
                        ),
                    }
                }
                "clear" => {
                    let _ = std::fs::remove_file(self.path(&session));
                    ok_outcome("", self.name(), format!("goal cleared for session {session}"), elapsed)
                }
                "complete" => {
                    match self.load(&session) {
                        Some(mut g) => {
                            g.completed = true;
                            self.save(&g);
                            ok_outcome("", self.name(), format!("goal completed: {}\n(marked done — set a new goal to continue)", g.text), elapsed)
                        }
                        None => ok_outcome("", self.name(), format!("no active goal for session {session} to complete"), elapsed),
                    }
                }
                other => ok_outcome("", self.name(), format!("unknown action {other:?} — use set | get | clear | complete"), elapsed),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_goal_test_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn set_get_clear_roundtrip() {
        let d = tmp_dir("roundtrip");
        let t = GoalTool::new(d.clone());
        assert!(t.load("s1").is_none(), "no goal initially");

        let args = json!({"action": "set", "text": "ship the login API", "session": "s1"});
        let out = t.execute(args).await;
        assert!(out.ok, "set succeeds");
        assert!(out.output.contains("goal set"));

        let g = t.load("s1").unwrap();
        assert_eq!(g.text, "ship the login API");
        assert!(!g.completed);

        // get shows it
        let args = json!({"action": "get", "session": "s1"});
        let out = t.execute(args).await;
        assert!(out.output.contains("ship the login API"));

        // complete marks it; load then returns None (completed filtered out)
        let args = json!({"action": "complete", "session": "s1"});
        let out = t.execute(args).await;
        assert!(out.ok);
        assert!(t.load("s1").is_none(), "completed goal is no longer active");

        // clear removes the file entirely
        let args = json!({"action": "set", "text": "second goal", "session": "s1"});
        t.execute(args).await;
        assert!(t.load("s1").is_some());
        let args = json!({"action": "clear", "session": "s1"});
        t.execute(args).await;
        assert!(t.load("s1").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn empty_set_reports_usage() {
        let d = tmp_dir("empty");
        let t = GoalTool::new(d.clone());
        let args = json!({"action": "set", "session": "s1"});
        let out = t.execute(args).await;
        assert!(out.output.contains("usage"), "empty goal text -> usage: {}", out.output);
        let _ = std::fs::remove_dir_all(&d);
    }
}
