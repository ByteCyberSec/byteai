//! Session persistence: JSON files under <data>/sessions/ (Phase 1 file-based;
//! SQLite+FTS arrives with the memory phase).

use std::path::PathBuf;

use anyhow::{Context, Result};
use byteai_types::{Message, SessionFile, Usage};
use chrono::Utc;

pub fn sessions_dir() -> PathBuf {
    crate::config::data_dir().join("sessions")
}

pub fn new_id() -> String {
    Utc::now().format("session_%Y%m%d_%H%M%S").to_string()
}

pub fn save(session: &SessionFile) -> Result<PathBuf> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir).context("create sessions dir")?;
    let path = dir.join(format!("{}.json", session.id));
    let json = serde_json::to_string_pretty(session).context("serialize session")?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub fn load(id: &str) -> Result<SessionFile> {
    let path = sessions_dir().join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn list() -> Result<Vec<SessionFile>> {
    let dir = sessions_dir();
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir).context("read sessions dir")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(".json")
            && let Ok(s) = load(id) {
                out.push(s);
            }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

pub fn from_agent(model: &str, provider: &str, messages: Vec<Message>, usage: Usage) -> SessionFile {
    let now = Utc::now().to_rfc3339();
    SessionFile {
        id: new_id(),
        created_at: now.clone(),
        updated_at: now,
        model: model.into(),
        provider: provider.into(),
        messages,
        usage,
    }
}
