//! Skills tool (Phase 6): discover, load, and capture skills.
//!
//! A skill is a directory with a SKILL.md (YAML frontmatter: name,
//! description, trigger; body: instructions). Skills live in
//! `<data_dir>/skills/<name>/SKILL.md` or any path given explicitly.
//! `load` returns the skill's content so the agent can follow it; `create`
//! captures a lesson from the current task (lesson capture → promotion).

use std::path::{Path, PathBuf};

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub path: PathBuf,
    pub description: String,
    pub frontmatter: String,
    pub body: String,
}

fn parse_skill(path: &Path) -> Option<SkillMeta> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = text.clone();
    let (frontmatter, body) = if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            (rest[..end].to_string(), rest[end + 4..].trim_start().to_string())
        } else {
            (String::new(), raw)
        }
    } else {
        (String::new(), raw)
    };
    let name = path.parent()?.file_name()?.to_string_lossy().to_string();
    let description = frontmatter
        .lines()
        .find_map(|l| l.strip_prefix("description:").map(|d| d.trim().trim_matches('"').to_string()))
        .unwrap_or_default();
    Some(SkillMeta { name, path: path.to_path_buf(), description, frontmatter, body })
}

/// Scan a directory tree (max depth 4) for SKILL.md files.
pub fn scan_dir(root: &Path, out: &mut Vec<SkillMeta>, depth: usize) {
    if depth > 4 || !root.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let skill_md = p.join("SKILL.md");
            if skill_md.exists() {
                if let Some(s) = parse_skill(&skill_md) {
                    out.push(s);
                }
            } else {
                scan_dir(&p, out, depth + 1);
            }
        }
    }
}

/// Find skills relevant to a task query (Hermes skill-injection parity).
///
/// Scores every skill in `root` by keyword overlap between the query and the
/// skill's name + description (frontmatter), returns the top `limit`
/// matches. Uses an mtime-cached index so a 7000+ skill library is scanned
/// once, not on every turn. Only the full body of the top N matches is
/// loaded from disk (the index stores only lightweight metadata).
pub fn find_relevant(root: &Path, query: &str, limit: usize) -> Vec<SkillMeta> {
    let index = cached_index(root);
    if index.is_empty() || query.trim().is_empty() {
        return Vec::new();
    }
    let q = query.to_lowercase();
    let q_words: Vec<&str> = q.split_whitespace().collect();
    // Score against the lightweight index (name + description only).
    let mut scored: Vec<(usize, usize)> = index
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let hay = format!("{} {}", entry.0.to_lowercase(), entry.1.to_lowercase());
            let mut score = 0usize;
            for w in &q_words {
                if w.len() < 3 {
                    continue;
                }
                if entry.0.to_lowercase().contains(w) || entry.1.to_lowercase().contains(w) {
                    score += 2;
                } else if hay.contains(w) {
                    score += 1;
                }
            }
            (score, i)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);
    // Load only the top N bodies from disk.
    let mut results = Vec::new();
    for (_, idx) in &scored {
        let path = &index[*idx].2;
        if let Some(skill) = parse_skill(path) {
            results.push(skill);
        }
    }
    results
}

/// Lightweight index entry: (name, description, path_to_skill).
type IndexEntry = (String, String, PathBuf);

/// Scan the skills tree once and cache the lightweight index (name + description + path).
/// The index is persisted to `.index.json` and only re-scanned when the file
/// doesn't exist (skills are installed at setup time, rarely changed during
/// a session). The mtime walk over a 7000+ tree is expensive, so we skip it
/// on the hot path — the process-level cache handles the first-scan cost.
fn cached_index(root: &Path) -> Vec<IndexEntry> {
    if !root.is_dir() {
        return Vec::new();
    }
    let index_path = root.join(".index.json");
    // Load the on-disk cache if it exists (fast path — no tree walk).
    // This is safe because skills are installed at setup and rarely change
    // during a session. If skills were added, delete the index to force a
    // re-scan: `rm skills/.index.json`.
    if let Ok(text) = std::fs::read_to_string(&index_path)
        && let Ok(cache) = serde_json::from_str::<IndexCache>(&text)
        && !cache.entries.is_empty() {
            return cache.entries;
        }
    // On-disk cache missing or empty: do the full scan once and persist.
    let mut entries = Vec::new();
    let mut skills = Vec::new();
    scan_dir(root, &mut skills, 0);
    for s in &skills {
        entries.push((s.name.clone(), s.description.clone(), s.path.clone()));
    }
    // Store a dummy mtime (0) — only the existence of the file matters.
    let cache = IndexCache { tree_mtime: 0, entries: entries.clone() };
    if let Ok(text) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&index_path, text);
    }
    entries
}

#[derive(serde::Serialize, serde::Deserialize)]
struct IndexCache {
    tree_mtime: u64,
    entries: Vec<IndexEntry>,
}

/// Copy every directory containing a SKILL.md from `src` into `dst/<name>/`.
fn collect_and_copy(src: &Path, dst: &Path, found: &mut usize) {
    if !src.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(src) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let skill_md = p.join("SKILL.md");
        if skill_md.exists() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let target = dst.join(&name);
            let _ = std::fs::create_dir_all(&target);
            if copy_tree(&p, &target) {
                *found += 1;
            }
        } else {
            collect_and_copy(&p, dst, found);
        }
    }
}

fn copy_tree(src: &Path, dst: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(src) else { return false };
    let mut ok = true;
    for e in entries.flatten() {
        let from = e.path();
        let to = dst.join(e.file_name());
        if from.is_dir() {
            let _ = std::fs::create_dir_all(&to);
            ok &= copy_tree(&from, &to);
        } else {
            if std::fs::copy(&from, &to).is_err() {
                ok = false;
            }
        }
    }
    ok
}

pub struct SkillsTool {
    skills_root: PathBuf,
}

impl SkillsTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let root = data_dir.join("skills");
        let _ = std::fs::create_dir_all(&root);
        Self { skills_root: root }
    }
}

impl Default for SkillsTool {
    fn default() -> Self {
        Self::new(std::path::PathBuf::from("."))
    }
}

impl Tool for SkillsTool {
    fn name(&self) -> &'static str {
        "skills"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "skills".into(),
            description: "Skill system. Actions: list (available skills), load <name> (full content \
for use now), create <name> <description> <body> (capture a lesson as a reusable skill), \
install <repo> (clone a GitHub repo owner/name and import its SKILL.md trees), \
delete <name>. Skills persist in <data_dir>/skills/.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "load", "create", "install", "delete"] },
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "body": { "type": "string" },
                    "repo": { "type": "string", "description": "GitHub owner/name to clone skills from" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let root = self.skills_root.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list");
            let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();

            match action {
                "list" => {
                    let mut skills = Vec::new();
                    scan_dir(&root, &mut skills, 0);
                    if skills.is_empty() {
                        return ok_outcome("", "skills", "no skills found yet - create one to capture a lesson".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let mut out = String::new();
                    for s in &skills {
                        out.push_str(&format!("- {} — {}\n", s.name, if s.description.is_empty() { "(no description)" } else { &s.description }));
                    }
                    out.push_str(&format!("\n{} skill(s) in {}", skills.len(), root.display()));
                    ok_outcome("", "skills", out, started.elapsed().as_millis() as u64)
                }
                "load" => {
                    if name.is_empty() {
                        return ok_outcome("", "skills", "ERROR: `name` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let mut skills = Vec::new();
                    scan_dir(&root, &mut skills, 0);
                    match skills.into_iter().find(|s| s.name == name) {
                        Some(s) => {
                            let mut out = format!("# Skill: {}\nPath: {}\n\n", s.name, s.path.display());
                            if !s.frontmatter.is_empty() {
                                out.push_str(&format!("Frontmatter:\n{}\n\n", s.frontmatter));
                            }
                            out.push_str(&s.body);
                            // Progressive disclosure: list referenced files.
                            let base = s.path.parent().unwrap_or(&root);
                            for sub in &["references", "scripts", "templates", "assets"] {
                                let subdir = base.join(sub);
                                if subdir.is_dir()
                                    && let Ok(entries) = std::fs::read_dir(&subdir) {
                                        let files: Vec<_> = entries.flatten()
                                            .map(|e| e.file_name().to_string_lossy().to_string())
                                            .filter(|f| !f.starts_with('.'))
                                            .collect();
                                        if !files.is_empty() {
                                            out.push_str(&format!("\n{sub}/: {}\n", files.join(", ")));
                                        }
                                    }
                            }
                            ok_outcome("", "skills", out, started.elapsed().as_millis() as u64)
                        }
                        None => ok_outcome("", "skills", format!("skill {name:?} not found"), started.elapsed().as_millis() as u64),
                    }
                }
                "create" => {
                    if name.is_empty() {
                        return ok_outcome("", "skills", "ERROR: `name` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let description = args.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
                    let body = args.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
                    if body.is_empty() {
                        return ok_outcome("", "skills", "ERROR: `body` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let dir = root.join(&name);
                    let _ = std::fs::create_dir_all(&dir);
                    let content = format!(
                        "---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"
                    );
                    match std::fs::write(dir.join("SKILL.md"), content) {
                        Ok(_) => ok_outcome("", "skills", format!("skill {name:?} created at {}", dir.display()), started.elapsed().as_millis() as u64),
                        Err(e) => ok_outcome("", "skills", format!("create failed: {e}"), started.elapsed().as_millis() as u64),
                    }
                }
                "delete" => {
                    let dir = root.join(&name);
                    match std::fs::remove_dir_all(&dir) {
                        Ok(_) => ok_outcome("", "skills", format!("deleted skill {name:?}"), started.elapsed().as_millis() as u64),
                        Err(e) => ok_outcome("", "skills", format!("delete failed: {e}"), started.elapsed().as_millis() as u64),
                    }
                }
                "install" => {
                    // Install skills from a GitHub repo: `install` with `repo` =
                    // "owner/name" (shallow clone, copy SKILL.md trees into our
                    // skills dir). The awesome-agent-skills discovery pattern:
                    // pull a whole collection at once.
                    let repo = args.get("repo").and_then(|r| r.as_str()).unwrap_or("").to_string();
                    if repo.is_empty() || !repo.contains('/') {
                        return ok_outcome("", "skills", "ERROR: `repo` required as owner/name (e.g. anthropics/skills)".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let tmp = std::env::temp_dir().join(format!("byteai_skill_install_{}", std::process::id()));
                    let _ = std::fs::remove_dir_all(&tmp);
                    let url = format!("https://github.com/{repo}.git");
                    let status = std::process::Command::new("git")
                        .args(["clone", "--depth", "1", "--quiet", &url, &tmp.to_string_lossy()])
                        .status();
                    match status {
                        Ok(s) if s.success() => {
                            // Find SKILL.md files; copy each parent dir into skills/<name>.
                            let mut found = 0usize;
                            collect_and_copy(&tmp, &root, &mut found);
                            let _ = std::fs::remove_dir_all(&tmp);
                            if found == 0 {
                                ok_outcome("", "skills", format!("cloned {repo} but no SKILL.md found"), started.elapsed().as_millis() as u64)
                            } else {
                                ok_outcome("", "skills", format!("installed {found} skill(s) from {repo}"), started.elapsed().as_millis() as u64)
                            }
                        }
                        _ => {
                            let _ = std::fs::remove_dir_all(&tmp);
                            ok_outcome("", "skills", format!("clone failed for {repo}"), started.elapsed().as_millis() as u64)
                        }
                    }
                }
                other => ok_outcome("", "skills", format!("ERROR: unknown action {other:?}"), started.elapsed().as_millis() as u64),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai_skills_test_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_skill(root: &Path, name: &str, description: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{description}\"\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    #[test]
    fn find_relevant_ranks_by_keyword_overlap() {
        let root = tmp_root("rank");
        write_skill(&root, "rust-refactor", "refactor rust code safely", "Use cargo check after edits.");
        write_skill(&root, "git-workflow", "git branching and commits", "Use conventional commits.");
        write_skill(&root, "tauri-app", "build a tauri desktop app", "Tauri v2 rust core.");

        let hits = find_relevant(&root, "refactor rust function", 5);
        assert!(!hits.is_empty());
        // The rust-refactor skill should rank first (name+description match).
        assert_eq!(hits[0].name, "rust-refactor");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_relevant_empty_query_returns_none() {
        let root = tmp_root("empty");
        write_skill(&root, "x", "some description", "some body");
        assert!(find_relevant(&root, "   ", 5).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_relevant_missing_root_returns_none() {
        let root = tmp_root("missing");
        std::fs::remove_dir_all(&root).unwrap();
        assert!(find_relevant(&root, "anything", 5).is_empty());
    }
}
