//! `notify` — send a notification to Slack (webhook) or any URL (generic webhook).
//!
//! Used by scheduled jobs so ByteAI can alert the user out-of-band. Slack uses
//! the standard Incoming Webhook JSON; generic webhooks receive the same JSON
//! body via POST.

use std::collections::HashMap;
use std::time::Instant;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct NotifyTool;

impl NotifyTool {
    fn webhooks_file(&self) -> std::path::PathBuf {
        // Webhook URLs are sensitive; keep them out of git and out of prompts.
        std::env::var("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".byteai").join("webhooks.json"))
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/byteai-webhooks.json"))
    }
}

impl Tool for NotifyTool {
    fn name(&self) -> &'static str {
        "notify"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "notify".into(),
            description: "Send a notification to Slack or a generic webhook. Register webhooks once with `notify register <name> <url>` (stored in ~/.byteai/webhooks.json, never echoed). Input: {action: register|list|send, name, text?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["register", "list", "send"]},
                    "name": {"type": "string", "description": "webhook name"},
                    "text": {"type": "string", "description": "message text (send only)"}
                },
                "required": ["action", "name"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let self2 = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("send").to_string();
            let name = args.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let text = args.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            let file = self2.webhooks_file();

            let out = match action.as_str() {
                "register" => {
                    let url = args.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                    if url.is_empty() {
                        "usage: notify register <name> <url>".to_string()
                    } else {
                        let mut map: HashMap<String, String> = std::fs::read_to_string(&file)
                            .ok()
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default();
                        map.insert(name.clone(), url);
                        if let Some(parent) = file.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match std::fs::write(&file, serde_json::to_string_pretty(&map).unwrap_or_default()) {
                            Ok(_) => format!("registered webhook `{name}` (url hidden)"),
                            Err(e) => format!("failed to write webhooks file: {e:#}"),
                        }
                    }
                }
                "list" => {
                    let map: HashMap<String, String> = std::fs::read_to_string(&file)
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    if map.is_empty() {
                        "no webhooks registered — `notify register <name> <url>`".to_string()
                    } else {
                        let mut names: Vec<&String> = map.keys().collect();
                        names.sort();
                        format!("registered webhooks:\n  {}", names.iter().map(|n| format!("{n} (url hidden)")).collect::<Vec<_>>().join("\n  "))
                    }
                }
                _ => {
                    let url = std::fs::read_to_string(&file)
                        .ok()
                        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
                        .and_then(|m| m.get(&name).cloned());
                    match url {
                        None => format!("no webhook named `{name}` — `notify register {name} <url>` first"),
                        Some(url) => {
                            if text.is_empty() {
                                "send requires a `text` payload".to_string()
                            } else {
                                send_webhook(&url, &text).await
                            }
                        }
                    }
                }
            };
            ok_outcome("", "notify", out, started.elapsed().as_millis() as u64)
        })
    }
}

/// POST the text to the webhook URL using curl (available on macOS, zero deps,
/// honors TLS, returns the HTTP status).
async fn send_webhook(url: &str, text: &str) -> String {
    let body = json!({ "text": text }).to_string();
    match Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST"])
        .arg(url)
        .args(["-H", "Content-Type: application/json"])
        .arg("--data")
        .arg(&body)
        .output()
        .await
    {
        Ok(o) if o.status.success() => {
            let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if code.starts_with('2') {
                format!("notification sent (HTTP {code})")
            } else {
                format!("webhook returned HTTP {code}")
            }
        }
        Ok(o) => format!("curl failed ({}): {}", o.status, String::from_utf8_lossy(&o.stderr)),
        Err(e) => format!("curl not available: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_without_registered_webhook_reports() {
        let t = NotifyTool;
        // Point at a fresh temp file so tests never touch real webhooks.
        let tmp = std::env::temp_dir().join(format!("byteai-webhooks-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let out = t.execute(json!({"action": "send", "name": "nope", "text": "hi"}));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let res = rt.block_on(out);
        assert!(res.output.contains("no webhook named"));
        let _ = std::fs::remove_file(&tmp);
    }
}