//! `backup` — checkpoint byteai (source + data) to a timestamped zip.
//!
//! Uses macOS `ditto -c -k` (built-in, zero deps) to create a zip. Writes to
//! ~/Desktop by default (override with `--dir`). Prunes backups older than a
//! configurable number of days (default 14) so you never fill the disk.
//!
//! Actions:
//!   * create [dir]      — zip ~/byteai + <data_dir> -> <dir>/byteai-backup-<ts>.zip
//!   * list              — show existing backups with sizes
//!   * prune [days]      — delete backups older than N days (default 14)

use std::path::PathBuf;
use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::Value;
use tokio::process::Command;

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct BackupTool {
    data_dir: PathBuf,
}

impl BackupTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
    /// Resolve the project dir to back up: prefer $BYTEAI_SRC if set, else the
    /// current working directory. Falls back to the data dir's parent.
    fn project_dir(&self) -> PathBuf {
        if let Ok(p) = std::env::var("BYTEAI_SRC") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        std::env::current_dir().unwrap_or_else(|_| self.data_dir.clone())
    }
}

impl Tool for BackupTool {
    fn name(&self) -> &'static str {
        "backup"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "backup".into(),
            description: "Checkpoint byteai (source dir + data) into a timestamped zip on the Desktop, list existing backups, prune old ones. Input: {action: create|list|prune, dir?, days?}.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "list", "prune"], "description": "create = zip source+data; list = show backups; prune = delete old ones"},
                    "dir": {"type": "string", "description": "destination dir for create (default: ~/Desktop)"},
                    "days": {"type": "integer", "description": "max age in days to keep for prune (default 14)"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let data_dir = self.data_dir.clone();
        let project = self.project_dir();
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("list").to_string();
            match action.as_str() {
                "create" => {
                    let dir = args.get("dir").and_then(Value::as_str)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| dirs_home().join("Desktop"));
                    std::fs::create_dir_all(&dir).ok();
                    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                    let dest = dir.join(format!("byteai-backup-{ts}.zip"));
                    // Stage both under a temp dir, then zip with the `zip` CLI
                    // (macOS built-in) run from inside the stage so the archive
                    // contains `project/` and `data/` at its root.
                    let stage = std::env::temp_dir().join(format!("byteai-backup-stage-{}", std::process::id()));
                    let _ = std::fs::remove_dir_all(&stage);
                    std::fs::create_dir_all(&stage).ok();
                    let mut out = String::new();
                    let mut failures: Vec<String> = Vec::new();
                    for (label, path) in [("project", &project), ("data", &data_dir)] {
                        let target = stage.join(label);
                        match copy_tree(path, &target) {
                            Ok(_) => {}
                            Err(e) => failures.push(format!("{label}: {e}")),
                        }
                    }
                    let res = Command::new("zip")
                        .arg("-r")
                        .arg("-q")
                        .arg(&dest)
                        .arg("project")
                        .arg("data")
                        .current_dir(&stage)
                        .output()
                        .await;
                    let _ = std::fs::remove_dir_all(&stage);
                    match res {
                        Ok(o) if o.status.success() => {
                            let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                            out.push_str(&format!(
                                "Backup created:\n  {}\n  {:.1} MB\n",
                                dest.display(),
                                size as f64 / 1_048_576.0
                            ));
                            if !failures.is_empty() {
                                out.push_str(&format!("  (partial: {})\n", failures.join("; ")));
                            }
                        }
                        Ok(o) => out.push_str(&format!(
                            "backup failed: ditto exited {}\n{}",
                            o.status,
                            String::from_utf8_lossy(&o.stderr)
                        )),
                        Err(e) => out.push_str(&format!("backup failed: {e:#}")),
                    }
                    ok_outcome("", "backup", out, started.elapsed().as_millis() as u64)
                }
                "list" => {
                    let dir = args.get("dir").and_then(Value::as_str)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| dirs_home().join("Desktop"));
                    let mut out = format!("Backups in {}:\n", dir.display());
                    match list_backups(&dir) {
                        Ok(items) if items.is_empty() => out.push_str("  (none)\n"),
                        Ok(items) => {
                            for (name, size, modified) in items {
                                let dt: chrono::DateTime<chrono::Local> = modified.into();
                                out.push_str(&format!("  {name}  {:>8.1} MB  {}\n", size as f64 / 1_048_576.0, dt.format("%Y-%m-%d %H:%M")));
                            }
                        }
                        Err(e) => out.push_str(&format!("  error: {e}\n")),
                    }
                    ok_outcome("", "backup", out, started.elapsed().as_millis() as u64)
                }
                "prune" => {
                    let dir = args.get("dir").and_then(Value::as_str)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| dirs_home().join("Desktop"));
                    let days = args.get("days").and_then(Value::as_u64).unwrap_or(14);
                    let cutoff = chrono::Local::now() - chrono::Duration::days(days as i64);
                    let mut removed = 0usize;
                    let mut out = String::new();
                    if let Ok(items) = list_backups(&dir) {
                        for (name, _, modified) in items {
                            let dt: chrono::DateTime<chrono::Local> = modified.into();
                            if dt < cutoff {
                                let p = dir.join(&name);
                                if std::fs::remove_file(&p).is_ok() {
                                    removed += 1;
                                }
                            }
                        }
                    }
                    out.push_str(&format!("pruned {removed} backup(s) older than {days} day(s)\n"));
                    ok_outcome("", "backup", out, started.elapsed().as_millis() as u64)
                }
                other => ok_outcome("", "backup", format!("unknown action {other:?} — use create|list|prune"), 0),
            }
        })
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Minimal recursive copy (skip heavy, regenerable dirs so backups stay fast
/// and small: build artifacts, git internals, the skills hub symlink, node deps).
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    // Skip symlinks entirely (a symlinked hub dir would be dereferenced and
    // explode the backup); known-heavy names are also skipped at any depth.
    let md = std::fs::symlink_metadata(from)?;
    if md.file_type().is_symlink() {
        return Ok(());
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            copy_tree(&entry.path(), &to.join(&name))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

const SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    ".apex",
    "skills",      // regenerable skill hub (could be hundreds of MB)
    "node_modules",
    "dist",
    "build",
    "vendor",
    ".cache",
];

fn list_backups(dir: &std::path::Path) -> std::io::Result<Vec<(String, u64, std::time::SystemTime)>> {
    let mut items = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("byteai-backup-") && name.ends_with(".zip") {
                if let Ok(md) = e.metadata() {
                    if let Ok(mt) = md.modified() {
                        items.push((name, md.len(), mt));
                    }
                }
            }
        }
    }
    items.sort_by(|a, b| b.2.cmp(&a.2));
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_backups_filters_other_files() {
        let dir = std::env::temp_dir().join(format!("backup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("byteai-backup-20260101-000000.zip"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let items = list_backups(&dir).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].0.starts_with("byteai-backup-"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
