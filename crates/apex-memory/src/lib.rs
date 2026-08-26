//! ByteAi memory store: SQLite + FTS5.
//!
//! Phase 5: working state (session log), notes (FTS), project wiki (markdown
//! pages), entity index. Backed by one SQLite database at data_dir/memory.db
//! with FTS5 virtual tables for full-text search. Pure stdlib + rusqlite; no
//! external services. Graceful: if FTS5 isn't available, LIKE search falls back.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

/// Memory entry kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Note,     // agent-written durable note
    Session,  // session message log
    Wiki,     // project wiki page
    Entity,   // extracted entity (project/term/person)
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Session => "session",
            Kind::Wiki => "wiki",
            Kind::Entity => "entity",
        }
    }
    pub fn from_str(s: &str) -> Kind {
        match s {
            "session" => Kind::Session,
            "wiki" => Kind::Wiki,
            "entity" => Kind::Entity,
            _ => Kind::Note,
        }
    }
}

pub struct Memory {
    conn: Connection,
    pub db_path: PathBuf,
}

impl Memory {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("memory.db");
        let conn = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                body TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_kind ON entries(kind);
            CREATE INDEX IF NOT EXISTS idx_title ON entries(title);
            CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
                title, body, tags, content='entries', content_rowid='id'
            );
            CREATE TRIGGER IF NOT EXISTS entries_ai AFTER INSERT ON entries BEGIN
                INSERT INTO entries_fts(rowid, title, body, tags)
                VALUES (new.id, new.title, new.body, new.tags);
            END;
            CREATE TRIGGER IF NOT EXISTS entries_ad AFTER DELETE ON entries BEGIN
                INSERT INTO entries_fts(entries_fts, rowid, title, body, tags)
                VALUES ('delete', old.id, old.title, old.body, old.tags);
            END;
            CREATE TRIGGER IF NOT EXISTS entries_au AFTER UPDATE ON entries BEGIN
                INSERT INTO entries_fts(entries_fts, rowid, title, body, tags)
                VALUES ('delete', old.id, old.title, old.body, old.tags);
                INSERT INTO entries_fts(rowid, title, body, tags)
                VALUES (new.id, new.title, new.body, new.tags);
            END;
            "#,
        )?;
        Ok(Self { conn, db_path })
    }

    /// Insert (or update by id if provided) an entry.
    pub fn upsert(&mut self, kind: Kind, title: &str, body: &str, tags: &[String], id: Option<i64>) -> Result<i64> {
        let tags = tags.join(",");
        match id {
            Some(id) => {
                self.conn
                    .execute(
                        "UPDATE entries SET kind=?1, title=?2, body=?3, tags=?4, updated_at=datetime('now') WHERE id=?5",
                        params![kind.as_str(), title, body, tags, id],
                    )
                    .context("update entry")?;
                Ok(id)
            }
            None => {
                self.conn
                    .execute(
                        "INSERT INTO entries(kind, title, body, tags) VALUES (?1, ?2, ?3, ?4)",
                        params![kind.as_str(), title, body, tags],
                    )
                    .context("insert entry")?;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    /// Full-text search over entries with optional kind filter.
    pub fn search(&self, query: &str, kind: Option<Kind>, limit: usize) -> Result<Vec<Entry>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.min(50);
        let mut sql = String::from(
            "SELECT e.id, e.kind, e.title, e.body, e.tags, e.created_at, e.updated_at
             FROM entries_fts f JOIN entries e ON e.id = f.rowid",
        );
        // FTS5 requires *-quoted terms for prefix; use raw query with escaped quotes.
        let fts_query = q.replace('"', "\"\"");
        sql.push_str(" WHERE entries_fts MATCH ?1");
        let mut args: Vec<String> = vec![format!("\"{fts_query}\"")];
        if let Some(k) = kind {
            sql.push_str(" AND e.kind = ?2");
            args.push(k.as_str().to_string());
        }
        sql.push_str(" ORDER BY rank LIMIT ?3");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![args[0].as_str(), args.get(1).map(|s| s.as_str()).unwrap_or(""), limit])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Entry {
                id: row.get(0)?,
                kind: Kind::from_str(&row.get::<_, String>(1)?),
                title: row.get(2)?,
                body: row.get(3)?,
                tags: row.get::<_, String>(4)?.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            });
        }
        if !out.is_empty() {
            return Ok(out);
        }
        // FTS fallback: plain LIKE search.
        let mut sql2 = String::from("SELECT id, kind, title, body, tags, created_at, updated_at FROM entries WHERE body LIKE ?1 OR title LIKE ?1");
        let mut args2: Vec<String> = vec![format!("%{q}%")];
        if let Some(k) = kind {
            sql2.push_str(" AND kind = ?2");
            args2.push(k.as_str().to_string());
        }
        sql2.push_str(" ORDER BY updated_at DESC LIMIT ?3");
        let mut stmt = self.conn.prepare(&sql2)?;
        let mut rows = stmt.query(params![args2[0].as_str(), args2.get(1).map(|s| s.as_str()).unwrap_or(""), limit])?;
        while let Some(row) = rows.next()? {
            out.push(Entry {
                id: row.get(0)?,
                kind: Kind::from_str(&row.get::<_, String>(1)?),
                title: row.get(2)?,
                body: row.get(3)?,
                tags: row.get::<_, String>(4)?.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            });
        }
        Ok(out)
    }

    /// List entries of a kind (newest first).
    pub fn list(&self, kind: Option<Kind>, limit: usize) -> Result<Vec<Entry>> {
        let limit = limit.min(100);
        let mut sql = String::from("SELECT id, kind, title, body, tags, created_at, updated_at FROM entries");
        let mut args: Vec<String> = Vec::new();
        if let Some(k) = kind {
            sql.push_str(" WHERE kind = ?1");
            args.push(k.as_str().to_string());
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![args.first().map(|s| s.as_str()).unwrap_or(""), limit])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Entry {
                id: row.get(0)?,
                kind: Kind::from_str(&row.get::<_, String>(1)?),
                title: row.get(2)?,
                body: row.get(3)?,
                tags: row.get::<_, String>(4)?.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            });
        }
        Ok(out)
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>> {
        let mut stmt = self.conn.prepare("SELECT id, kind, title, body, tags, created_at, updated_at FROM entries WHERE id=?1")?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Entry {
                id: row.get(0)?,
                kind: Kind::from_str(&row.get::<_, String>(1)?),
                title: row.get(2)?,
                body: row.get(3)?,
                tags: row.get::<_, String>(4)?.split(',').filter(|s| !s.is_empty()).map(String::from).collect(),
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })),
            None => Ok(None),
        }
    }

    pub fn delete(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM entries WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Session log helpers.
    pub fn log_session_message(&mut self, session_id: &str, role: &str, text: &str) -> Result<i64> {
        let body = format!("[{session_id}] [{role}] {text}");
        self.upsert(Kind::Session, &format!("session {session_id}"), &body, &[session_id.to_string(), role.to_string()], None)
    }

    pub fn session_messages(&self, session_id: &str, limit: usize) -> Result<Vec<Entry>> {
        self.search(session_id, Some(Kind::Session), limit)
    }

    /// Stats for doctor/health.
    pub fn stats(&self) -> Result<[(String, i64); 4]> {
        let mut out: [(String, i64); 4] = [("note".into(), 0), ("session".into(), 0), ("wiki".into(), 0), ("entity".into(), 0)];
        for (kind, count) in out.iter_mut() {
            let mut stmt = self.conn.prepare("SELECT COUNT(*) FROM entries WHERE kind=?1")?;
            *count = stmt.query_row(params![kind.as_str()], |r| r.get(0)).unwrap_or(0);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Quick smoke test that doesn't require FTS availability beyond bundled SQLite.
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(name: &str) -> (temp_dir, Memory) {
        let dir = std::env::temp_dir().join(format!("byteai_mem_test_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        let m = Memory::open(&dir).unwrap();
        (temp_dir(dir), m)
    }

    struct temp_dir(std::path::PathBuf);
    impl Drop for temp_dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn upsert_search_delete_roundtrip() {
        let (_d, mut m) = tmp_db("roundtrip");
        let id = m.upsert(Kind::Note, "test note", "the quick brown fox jumps", &["test".into()], None).unwrap();
        let got = m.get(id).unwrap().unwrap();
        assert_eq!(got.title, "test note");
        assert_eq!(got.kind, Kind::Note);

        let hits = m.search("brown fox", Some(Kind::Note), 10).unwrap();
        assert!(!hits.is_empty(), "FTS should find 'brown fox'");

        m.delete(id).unwrap();
        assert!(m.get(id).unwrap().is_none());
    }

    #[test]
    fn kinds_and_stats() {
        let (_d, mut m) = tmp_db("kinds");
        m.upsert(Kind::Wiki, "Wiki: byteai", "ByteAi architecture notes", &[String::from("wiki")], None).unwrap();
        m.upsert(Kind::Session, "sess-1", "[sess-1] [user] hello", &[String::from("sess-1")], None).unwrap();
        m.log_session_message("sess-1", "assistant", "hi there").unwrap();
        let stats = m.stats().unwrap();
        let wiki = stats.iter().find(|(k, _)| k == "wiki").map(|(_, c)| *c).unwrap();
        let sess = stats.iter().find(|(k, _)| k == "session").map(|(_, c)| *c).unwrap();
        assert_eq!(wiki, 1);
        assert_eq!(sess, 2);
        let msgs = m.session_messages("sess-1", 10).unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
