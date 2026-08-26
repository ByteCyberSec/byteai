//! Plugin system (deepseek-harness "everything is a plugin" pattern).
//!
//! A plugin is a `.toml` file in `<data_dir>/plugins/` declaring a tool:
//!
//!   [tool]
//!   name = "my-tool"
//!   description = "What it does"
//!   command = "python3 script.py"        # template; {args} = JSON args
//!   parameters = { type = "object", properties = { ... } }
//!
//! The plugin tool lists/loads them and runs a plugin's command with the
//! JSON args piped on stdin. Plugins are cheap, declarative, and safe-ish
//! (they run as subprocesses, exactly like the shell tool).

use std::path::{Path, PathBuf};

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::{BoxFuture, Tool, ok_outcome};

const PLUGIN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub description: String,
    pub command: String,
}

pub fn parse_plugin(path: &Path) -> Option<PluginMeta> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = toml_parse(&text)?;
    let tool = value.get("tool")?;
    Some(PluginMeta {
        name: tool.get("name")?.as_str()?.to_string(),
        description: tool.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
        command: tool.get("command")?.as_str()?.to_string(),
    })
}

/// Minimal TOML subset parser (name/value + inline table). Full TOML would
/// need the `toml` crate; keep the plugin format deliberately simple:
/// key = "string" lines inside [tool].
fn toml_parse(text: &str) -> Option<Value> {
    let mut in_tool = false;
    let mut map = serde_json::Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_tool = line == "[tool]";
            continue;
        }
        if !in_tool || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim().trim_matches('"').to_string();
            let v = v.trim();
            let val = if v.starts_with('{') {
                // inline table — keep as raw string
                Value::String(v.to_string())
            } else {
                let raw = v.trim_matches('"');
                // Unescape basic-string escapes: \" -> ", \\ -> \, \n -> newline.
                let mut out = String::with_capacity(raw.len());
                let mut chars = raw.chars();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        match chars.next() {
                            Some('"') => out.push('"'),
                            Some('\\') => out.push('\\'),
                            Some('n') => out.push('\n'),
                            Some('t') => out.push('\t'),
                            Some(other) => { out.push('\\'); out.push(other); }
                            None => out.push('\\'),
                        }
                    } else {
                        out.push(c);
                    }
                }
                Value::String(out)
            };
            map.insert(k, val);
        }
    }
    if map.is_empty() {
        return None;
    }
    let mut tool = serde_json::Map::new();
    for (k, v) in map {
        tool.insert(k, v);
    }
    Some(json!({ "tool": Value::Object(tool) }))
}

pub fn scan_plugins(dir: &Path) -> Vec<PluginMeta> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("toml") {
            if let Some(m) = parse_plugin(&p) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub struct PluginTool {
    plugins_dir: PathBuf,
}

impl PluginTool {
    pub fn new(data_dir: PathBuf) -> Self {
        let dir = data_dir.join("plugins");
        let _ = std::fs::create_dir_all(&dir);
        Self { plugins_dir: dir }
    }
}

impl Default for PluginTool {
    fn default() -> Self {
        Self::new(std::path::PathBuf::from("."))
    }
}

impl Tool for PluginTool {
    fn name(&self) -> &'static str {
        "plugin"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "plugin".into(),
            description: "Plugin system: list/run declarative TOML plugins in <data_dir>/plugins/. \
Each plugin is a .toml with [tool] name/description/command. `run <name>` executes the command \
with JSON args on stdin. Write your own plugin for any repeatable action.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "run"] },
                    "name": { "type": "string" },
                    "args": { "type": "object" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let dir = self.plugins_dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("list");
            match action {
                "list" => {
                    let plugins = scan_plugins(&dir);
                    if plugins.is_empty() {
                        return ok_outcome("", "plugin", format!("no plugins found in {} — drop a .toml there to add one", dir.display()), started.elapsed().as_millis() as u64);
                    }
                    let mut out = String::new();
                    for p in &plugins {
                        out.push_str(&format!("- {} — {}\n", p.name, if p.description.is_empty() { "(no description)" } else { &p.description }));
                    }
                    out.push_str(&format!("\n{} plugin(s) in {}", plugins.len(), dir.display()));
                    ok_outcome("", "plugin", out, started.elapsed().as_millis() as u64)
                }
                "run" => {
                    let name = args.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    if name.is_empty() {
                        return ok_outcome("", "plugin", "ERROR: `name` required".to_string(), started.elapsed().as_millis() as u64);
                    }
                    let plugins = scan_plugins(&dir);
                    let Some(p) = plugins.into_iter().find(|p| p.name == name) else {
                        return ok_outcome("", "plugin", format!("plugin {name:?} not found"), started.elapsed().as_millis() as u64);
                    };
                    // Execute via `sh -c <command>` so plugin authors get full
                    // shell syntax; expose args as UPPERCASE env vars.
                    let mut cmd = Command::new("sh");
                    cmd.arg("-c");
                    cmd.arg(&p.command);
                    if let Some(obj) = args.get("args").and_then(|a| a.as_object()) {
                        for (k, v) in obj {
                            let env_key = k.to_uppercase();
                            let env_val = v.as_str().unwrap_or(&v.to_string()).to_string();
                            cmd.env(env_key, env_val);
                        }
                    }
                    cmd.stdin(std::process::Stdio::null());
                    cmd.stdout(std::process::Stdio::piped());
                    cmd.stderr(std::process::Stdio::piped());

                    let result = timeout(PLUGIN_TIMEOUT, async {
                        let child = cmd.spawn()?;
                        child.wait_with_output().await
                    }).await;

                    match result {
                        Ok(Ok(out)) => {
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            let combined = if out.status.success() {
                                stdout.to_string()
                            } else {
                                format!("exit {}\n{stdout}\n{stderr}", out.status.code().unwrap_or(-1))
                            };
                            ok_outcome("", "plugin", combined, started.elapsed().as_millis() as u64)
                        }
                        Ok(Err(e)) => ok_outcome("", "plugin", format!("plugin error: {e}"), started.elapsed().as_millis() as u64),
                        Err(_) => ok_outcome("", "plugin", "plugin timed out".to_string(), started.elapsed().as_millis() as u64),
                    }
                }
                other => ok_outcome("", "plugin", format!("ERROR: unknown action {other:?}"), started.elapsed().as_millis() as u64),
            }
        })
    }
}
