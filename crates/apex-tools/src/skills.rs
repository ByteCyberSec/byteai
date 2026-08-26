//! Skills tool (Phase 6): discover, load, and capture skills.
//!
//! A skill is a directory with a SKILL.md (YAML frontmatter: name,
//! description, trigger; body: instructions). Skills live in
//! `<data_dir>/skills/<name>/SKILL.md` or any path given explicitly.
//! `load` returns the skill's content so the agent can follow it; `create`
//! captures a lesson from the current task (lesson capture → promotion).

use std::path::{Path, PathBuf};

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Debug, Clone)]
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
                                if subdir.is_dir() {
                                    if let Ok(entries) = std::fs::read_dir(&subdir) {
                                        let files: Vec<_> = entries.flatten()
                                            .map(|e| e.file_name().to_string_lossy().to_string())
                                            .filter(|f| !f.starts_with('.'))
                                            .collect();
                                        if !files.is_empty() {
                                            out.push_str(&format!("\n{sub}/: {}\n", files.join(", ")));
                                        }
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
