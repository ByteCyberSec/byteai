//! ByteAi native memory hub — the TencentDB Agent Memory layered model (L0–L3)
//! implemented INSIDE byteai's own SQLite store. No external service.
//!
//! Layers (matching TencentDB Agent Memory):
//! - L0: raw conversation messages (session-scoped)
//! - L1: atomic memories (episodic / persona / instruction) with FTS search
//! - L2: scenario files (path + content + summary)
//! - L3: core persona (the agent's durable identity)
//! - Skills memory: team-scoped SKILL.md entries, searchable by name/content
//!
//! All data lives in the same memory.db as the classic entries store, so it is
//! fully persistent, local-first, and survives restarts. Graceful: if FTS5 is
//! unavailable, LIKE search falls back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

/// Conversation message (L0).
#[derive(Debug, Clone)]
pub struct ConvMsg {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

/// Atomic memory (L1).
#[derive(Debug, Clone)]
pub struct Atomic {
    pub id: i64,
    pub mem_type: String, // episodic | persona | instruction
    pub content: String,
    pub background: Option<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Scenario file (L2).
#[derive(Debug, Clone)]
pub struct Scenario {
    pub path: String,
    pub content: String,
    pub summary: Option<String>,
    pub version: i64,
    pub updated_at: String,
}

/// Core persona (L3).
#[derive(Debug, Clone)]
pub struct Core {
    pub content: String,
    pub version: i64,
    pub updated_at: String,
}

/// Team-scoped skill memory entry.
#[derive(Debug, Clone)]
pub struct HubSkill {
    pub name: String,
    pub content: String,
    pub version: i64,
    pub updated_at: String,
}

/// Native layered memory hub. Opens the same SQLite DB as the classic store
/// (data_dir/memory.db) and owns the L0–L3 + skill tables.
pub struct MemoryHub {
    conn: Connection,
    pub db_path: PathBuf,
}

impl MemoryHub {
    /// Open (or create) the hub tables in the given data dir.
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("memory.db");
        let conn = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            -- L0: conversation messages
            CREATE TABLE IF NOT EXISTS tdai_convs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_tdai_conv_session ON tdai_convs(session_id);
            CREATE VIRTUAL TABLE IF NOT EXISTS tdai_convs_fts USING fts5(
                content, content='tdai_convs', content_rowid='id'
            );
            CREATE TRIGGER IF NOT EXISTS tdai_convs_ai AFTER INSERT ON tdai_convs BEGIN
                INSERT INTO tdai_convs_fts(rowid, content) VALUES (new.id, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS tdai_convs_ad AFTER DELETE ON tdai_convs BEGIN
                INSERT INTO tdai_convs_fts(tdai_convs_fts, rowid, content) VALUES ('delete', old.id, old.content);
            END;

            -- L1: atomic memories (episodic / persona / instruction)
            CREATE TABLE IF NOT EXISTS tdai_atomics (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mem_type TEXT NOT NULL DEFAULT 'episodic',
                content TEXT NOT NULL,
                background TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_tdai_atomic_type ON tdai_atomics(mem_type);
            CREATE VIRTUAL TABLE IF NOT EXISTS tdai_atomics_fts USING fts5(
                content, background, content='tdai_atomics', content_rowid='id'
            );
            CREATE TRIGGER IF NOT EXISTS tdai_atomics_ai AFTER INSERT ON tdai_atomics BEGIN
                INSERT INTO tdai_atomics_fts(rowid, content, background) VALUES (new.id, new.content, new.background);
            END;
            CREATE TRIGGER IF NOT EXISTS tdai_atomics_ad AFTER DELETE ON tdai_atomics BEGIN
                INSERT INTO tdai_atomics_fts(tdai_atomics_fts, rowid, content, background) VALUES ('delete', old.id, old.content, old.background);
            END;
            CREATE TRIGGER IF NOT EXISTS tdai_atomics_au AFTER UPDATE ON tdai_atomics BEGIN
                INSERT INTO tdai_atomics_fts(tdai_atomics_fts, rowid, content, background) VALUES ('delete', old.id, old.content, old.background);
                INSERT INTO tdai_atomics_fts(rowid, content, background) VALUES (new.id, new.content, new.background);
            END;

            -- L2: scenario files
            CREATE TABLE IF NOT EXISTS tdai_scenarios (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL,
                summary TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- L3: core persona (single row, id=1)
            CREATE TABLE IF NOT EXISTS tdai_core (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                content TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Skills memory (team-scoped SKILL.md entries)
            CREATE TABLE IF NOT EXISTS tdai_skills (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                content TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS tdai_skills_fts USING fts5(
                name, content, content='tdai_skills', content_rowid='id'
            );
            CREATE TRIGGER IF NOT EXISTS tdai_skills_ai AFTER INSERT ON tdai_skills BEGIN
                INSERT INTO tdai_skills_fts(rowid, name, content) VALUES (new.id, new.name, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS tdai_skills_ad AFTER DELETE ON tdai_skills BEGIN
                INSERT INTO tdai_skills_fts(tdai_skills_fts, rowid, name, content) VALUES ('delete', old.id, old.name, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS tdai_skills_au AFTER UPDATE ON tdai_skills BEGIN
                INSERT INTO tdai_skills_fts(tdai_skills_fts, rowid, name, content) VALUES ('delete', old.id, old.name, old.content);
                INSERT INTO tdai_skills_fts(rowid, name, content) VALUES (new.id, new.name, new.content);
            END;
            "#,
        )?;
        Ok(Self { conn, db_path })
    }

    // ── L0: conversations ──────────────────────────────────────────────

    /// Append messages to an L0 conversation session.
    pub fn conversation_add(&mut self, session_id: &str, messages: &[(&str, &str)]) -> Result<i64> {
        let mut added = 0;
        for (role, content) in messages {
            self.conn.execute(
                "INSERT INTO tdai_convs(session_id, role, content) VALUES (?1, ?2, ?3)",
                params![session_id, role, content],
            )?;
            added += 1;
        }
        Ok(added)
    }

    /// Query L0 messages for a session (newest first).
    pub fn conversation_query(&self, session_id: &str, limit: usize) -> Result<Vec<ConvMsg>> {
        let limit = limit.min(100);
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, created_at FROM tdai_convs \
             WHERE session_id=?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let mut rows = stmt.query(params![session_id, limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(ConvMsg {
                id: r.get(0)?,
                session_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                created_at: r.get(4)?,
            });
        }
        Ok(out)
    }

    /// Search L0 conversation messages across all sessions.
    pub fn conversation_search(&self, query: &str, limit: usize) -> Result<Vec<ConvMsg>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.min(100);
        let fts_query = q.replace('"', "\"\"");
        let mut out = Vec::new();
        // FTS first, then LIKE fallback.
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT c.id, c.session_id, c.role, c.content, c.created_at \
             FROM tdai_convs_fts f JOIN tdai_convs c ON c.id = f.rowid \
             WHERE tdai_convs_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        ) {
            let mut rows = stmt.query(params![format!("\"{fts_query}\""), limit])?;
            while let Some(r) = rows.next()? {
                out.push(ConvMsg {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    role: r.get(2)?,
                    content: r.get(3)?,
                    created_at: r.get(4)?,
                });
            }
        }
        if out.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT id, session_id, role, content, created_at FROM tdai_convs \
                 WHERE content LIKE ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![format!("%{q}%"), limit])?;
            while let Some(r) = rows.next()? {
                out.push(ConvMsg {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    role: r.get(2)?,
                    content: r.get(3)?,
                    created_at: r.get(4)?,
                });
            }
        }
        Ok(out)
    }

    // ── L1: atomics ────────────────────────────────────────────────────

    /// Write an atomic memory (create, or version-bump an identical one).
    pub fn atomic_write(&mut self, mem_type: &str, content: &str, background: Option<&str>) -> Result<i64> {
        let mem_type = match mem_type {
            "persona" | "instruction" => mem_type,
            _ => "episodic",
        };
        // Dedup: if the same content already exists, bump version + touch.
        if let Ok(existing) = self.conn.query_row(
            "SELECT id FROM tdai_atomics WHERE content=?1 AND mem_type=?2 LIMIT 1",
            params![content, mem_type],
            |r| r.get::<_, i64>(0),
        ) {
            self.conn.execute(
                "UPDATE tdai_atomics SET version=version+1, updated_at=datetime('now') WHERE id=?1",
                params![existing],
            )?;
            return Ok(existing);
        }
        self.conn.execute(
            "INSERT INTO tdai_atomics(mem_type, content, background) VALUES (?1, ?2, ?3)",
            params![mem_type, content, background],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// List atomics (optionally filtered by type, newest first).
    pub fn atomic_list(&self, mem_type: Option<&str>, limit: usize) -> Result<Vec<Atomic>> {
        let limit = limit.min(100);
        let mut sql = String::from(
            "SELECT id, mem_type, content, background, version, created_at, updated_at FROM tdai_atomics",
        );
        let mut args: Vec<String> = Vec::new();
        if let Some(t) = mem_type {
            sql.push_str(" WHERE mem_type=?1");
            args.push(t.to_string());
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![args.first().map(|s| s.as_str()).unwrap_or(""), limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(Atomic {
                id: r.get(0)?,
                mem_type: r.get(1)?,
                content: r.get(2)?,
                background: r.get(3)?,
                version: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            });
        }
        Ok(out)
    }

    /// Search atomics (FTS + LIKE fallback).
    pub fn atomic_search(&self, query: &str, limit: usize) -> Result<Vec<Atomic>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.min(100);
        let fts_query = q.replace('"', "\"\"");
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT a.id, a.mem_type, a.content, a.background, a.version, a.created_at, a.updated_at \
             FROM tdai_atomics_fts f JOIN tdai_atomics a ON a.id = f.rowid \
             WHERE tdai_atomics_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        ) {
            let mut rows = stmt.query(params![format!("\"{fts_query}\""), limit])?;
            while let Some(r) = rows.next()? {
                out.push(Atomic {
                    id: r.get(0)?,
                    mem_type: r.get(1)?,
                    content: r.get(2)?,
                    background: r.get(3)?,
                    version: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                });
            }
        }
        if out.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT id, mem_type, content, background, version, created_at, updated_at FROM tdai_atomics \
                 WHERE content LIKE ?1 OR background LIKE ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![format!("%{q}%"), limit])?;
            while let Some(r) = rows.next()? {
                out.push(Atomic {
                    id: r.get(0)?,
                    mem_type: r.get(1)?,
                    content: r.get(2)?,
                    background: r.get(3)?,
                    version: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                });
            }
        }
        Ok(out)
    }

    // ── L2: scenarios ──────────────────────────────────────────────────

    /// Write (create or update) a scenario file.
    pub fn scenario_write(&mut self, path: &str, content: &str, summary: Option<&str>) -> Result<()> {
        if let Ok(_existing) = self.conn.query_row(
            "SELECT id FROM tdai_scenarios WHERE path=?1",
            params![path],
            |r| r.get::<_, i64>(0),
        ) {
            self.conn.execute(
                "UPDATE tdai_scenarios SET content=?2, summary=?3, version=version+1, updated_at=datetime('now') WHERE path=?1",
                params![path, content, summary],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO tdai_scenarios(path, content, summary) VALUES (?1, ?2, ?3)",
                params![path, content, summary],
            )?;
        }
        Ok(())
    }

    /// Read a scenario file.
    pub fn scenario_read(&self, path: &str) -> Result<Option<Scenario>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, content, summary, version, updated_at FROM tdai_scenarios WHERE path=?1",
        )?;
        let mut rows = stmt.query(params![path])?;
        match rows.next()? {
            Some(r) => Ok(Some(Scenario {
                path: r.get(0)?,
                content: r.get(1)?,
                summary: r.get(2)?,
                version: r.get(3)?,
                updated_at: r.get(4)?,
            })),
            None => Ok(None),
        }
    }

    /// List scenario files.
    pub fn scenario_ls(&self) -> Result<Vec<Scenario>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, content, summary, version, updated_at FROM tdai_scenarios ORDER BY path",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(Scenario {
                path: r.get(0)?,
                content: r.get(1)?,
                summary: r.get(2)?,
                version: r.get(3)?,
                updated_at: r.get(4)?,
            });
        }
        Ok(out)
    }

    // ── L3: core persona ───────────────────────────────────────────────

    /// Write the core persona (id=1, upsert).
    pub fn core_write(&mut self, content: &str) -> Result<()> {
        if let Ok(_existing) = self.conn.query_row(
            "SELECT id FROM tdai_core WHERE id=1",
            [],
            |r| r.get::<_, i64>(0),
        ) {
            self.conn.execute(
                "UPDATE tdai_core SET content=?1, version=version+1, updated_at=datetime('now') WHERE id=1",
                params![content],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO tdai_core(id, content) VALUES (1, ?1)",
                params![content],
            )?;
        }
        Ok(())
    }

    /// Read the core persona.
    pub fn core_read(&self) -> Result<Option<Core>> {
        let mut stmt = self.conn.prepare(
            "SELECT content, version, updated_at FROM tdai_core WHERE id=1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Ok(Some(Core {
                content: r.get(0)?,
                version: r.get(1)?,
                updated_at: r.get(2)?,
            })),
            None => Ok(None),
        }
    }

    // ── Skills memory ──────────────────────────────────────────────────

    /// Add or update a team-scoped skill (SKILL.md).
    pub fn skill_upsert(&mut self, name: &str, content: &str) -> Result<()> {
        if let Ok(_existing) = self.conn.query_row(
            "SELECT id FROM tdai_skills WHERE name=?1",
            params![name],
            |r| r.get::<_, i64>(0),
        ) {
            self.conn.execute(
                "UPDATE tdai_skills SET content=?2, version=version+1, updated_at=datetime('now') WHERE name=?1",
                params![name, content],
            )?;
        } else {
            self.conn.execute(
                "INSERT INTO tdai_skills(name, content) VALUES (?1, ?2)",
                params![name, content],
            )?;
        }
        Ok(())
    }

    /// List skills.
    pub fn skill_list(&self, limit: usize) -> Result<Vec<HubSkill>> {
        let limit = limit.min(100);
        let mut stmt = self.conn.prepare(
            "SELECT name, content, version, updated_at FROM tdai_skills ORDER BY name LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(HubSkill {
                name: r.get(0)?,
                content: r.get(1)?,
                version: r.get(2)?,
                updated_at: r.get(3)?,
            });
        }
        Ok(out)
    }

    /// Search skills by name/content.
    pub fn skill_search(&self, query: &str, limit: usize) -> Result<Vec<HubSkill>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.min(50);
        let fts_query = q.replace('"', "\"\"");
        let mut out = Vec::new();
        if let Ok(mut stmt) = self.conn.prepare(
            "SELECT s.name, s.content, s.version, s.updated_at \
             FROM tdai_skills_fts f JOIN tdai_skills s ON s.id = f.rowid \
             WHERE tdai_skills_fts MATCH ?1 ORDER BY rank LIMIT ?2",
        ) {
            let mut rows = stmt.query(params![format!("\"{fts_query}\""), limit])?;
            while let Some(r) = rows.next()? {
                out.push(HubSkill {
                    name: r.get(0)?,
                    content: r.get(1)?,
                    version: r.get(2)?,
                    updated_at: r.get(3)?,
                });
            }
        }
        if out.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT name, content, version, updated_at FROM tdai_skills \
                 WHERE name LIKE ?1 OR content LIKE ?1 ORDER BY name LIMIT ?2",
            )?;
            let mut rows = stmt.query(params![format!("%{q}%"), limit])?;
            while let Some(r) = rows.next()? {
                out.push(HubSkill {
                    name: r.get(0)?,
                    content: r.get(1)?,
                    version: r.get(2)?,
                    updated_at: r.get(3)?,
                });
            }
        }
        Ok(out)
    }

    // ── Stats ──────────────────────────────────────────────────────────

    /// Per-layer counts for status output.
    pub fn stats(&self) -> Result<[(String, i64); 5]> {
        let labels = [
            ("l0_conversations", "SELECT COUNT(*) FROM tdai_convs"),
            ("l1_atomics", "SELECT COUNT(*) FROM tdai_atomics"),
            ("l2_scenarios", "SELECT COUNT(*) FROM tdai_scenarios"),
            ("l3_core", "SELECT COUNT(*) FROM tdai_core"),
            ("skills", "SELECT COUNT(*) FROM tdai_skills"),
        ];
        let mut out: [(String, i64); 5] = std::array::from_fn(|_| (String::new(), 0));
        for (i, (label, sql)) in labels.iter().enumerate() {
            out[i] = ((*label).to_string(), self.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_hub(name: &str) -> (TempDir, MemoryHub) {
        let dir = std::env::temp_dir().join(format!("byteai_hub_test_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        let h = MemoryHub::open(&dir).unwrap();
        (TempDir(dir), h)
    }

    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn l0_conversation_roundtrip_and_search() {
        let (_d, mut h) = tmp_hub("l0");
        h.conversation_add("s1", &[("user", "the quick brown fox"), ("assistant", "jumps over")]).unwrap();
        let msgs = h.conversation_query("s1", 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "assistant"); // newest first

        let hits = h.conversation_search("brown fox", 10).unwrap();
        assert!(!hits.is_empty(), "FTS should find 'brown fox' in L0");
    }

    #[test]
    fn l1_atomics_types_dedup_and_search() {
        let (_d, mut h) = tmp_hub("l1");
        let a = h.atomic_write("persona", "user prefers local-first", Some("from chat")).unwrap();
        // Same content → dedup + version bump, same id.
        let b = h.atomic_write("persona", "user prefers local-first", Some("from chat")).unwrap();
        assert_eq!(a, b);
        let list = h.atomic_list(Some("persona"), 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].version, 2);

        h.atomic_write("instruction", "always verify before finishing", None).unwrap();
        let hits = h.atomic_search("verify before finishing", 10).unwrap();
        assert!(!hits.is_empty(), "FTS should find instruction");
        let epi = h.atomic_list(Some("episodic"), 10).unwrap();
        assert!(epi.is_empty(), "episodic filter excludes persona/instruction");
    }

    #[test]
    fn l2_scenarios_write_read_ls() {
        let (_d, mut h) = tmp_hub("l2");
        h.scenario_write("prefs.md", "# Preferences\nlocal-first", Some("coding prefs")).unwrap();
        h.scenario_write("prefs.md", "# Preferences\nlocal-first + 60fps", Some("coding prefs")).unwrap();
        let s = h.scenario_read("prefs.md").unwrap().unwrap();
        assert!(s.content.contains("60fps"));
        assert_eq!(s.version, 2);
        let all = h.scenario_ls().unwrap();
        assert_eq!(all.len(), 1);
        assert!(h.scenario_read("missing.md").unwrap().is_none());
    }

    #[test]
    fn l3_core_upsert() {
        let (_d, mut h) = tmp_hub("l3");
        assert!(h.core_read().unwrap().is_none());
        h.core_write("I am ByteAi, an autonomous coding agent").unwrap();
        h.core_write("I am ByteAi, an autonomous coding agent that remembers").unwrap();
        let c = h.core_read().unwrap().unwrap();
        assert!(c.content.contains("remembers"));
        assert_eq!(c.version, 2);
    }

    #[test]
    fn skills_memory_upsert_list_search() {
        let (_d, mut h) = tmp_hub("skills");
        h.skill_upsert("code-review", "# Code Review\nReview PRs carefully").unwrap();
        h.skill_upsert("code-review", "# Code Review\nReview PRs carefully and thoroughly").unwrap();
        let list = h.skill_list(10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].version, 2);
        let hits = h.skill_search("thoroughly", 10).unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = std::env::temp_dir().join(format!("byteai_hub_persist_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let mut h = MemoryHub::open(&dir).unwrap();
            h.conversation_add("sess-keep", &[("user", "remember this fact")]).unwrap();
            h.core_write("durable persona").unwrap();
        }
        // Reopen — data must survive (persistence proof).
        let h = MemoryHub::open(&dir).unwrap();
        let msgs = h.conversation_search("remember this fact", 5).unwrap();
        assert!(!msgs.is_empty(), "L0 survived reopen");
        let c = h.core_read().unwrap().unwrap();
        assert_eq!(c.content, "durable persona");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
