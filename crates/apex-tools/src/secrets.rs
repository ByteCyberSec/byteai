//! `secrets` — store/read/list/delete secrets.
//!
//! Secrets are stored as chmod-600 files under <data_dir>/secrets/ (never in
//! git, never printed on write). Values can optionally be injected into the
//! environment of a follow-up task via `env: true`, which returns a
//! <cmd>KEY=VALUE</cmd> token the agent can export before running a command.

use std::path::PathBuf;
use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::fs;

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct SecretsTool {
    data_dir: PathBuf,
}

impl SecretsTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
    fn secrets_dir(&self) -> PathBuf {
        self.data_dir.join("secrets")
    }
}

impl Tool for SecretsTool {
    fn name(&self) -> &'static str {
        "secrets"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "secrets".into(),
            description: "Securely store, read, list, and delete secrets (chmod-600 files under the byteai data dir, never committed to git). Input: {action: set|get|list|delete, key, value?, env?}. Values are never echoed back on write.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["set", "get", "list", "delete"]},
                    "key": {"type": "string", "description": "secret name"},
                    "value": {"type": "string", "description": "secret value (set only)"},
                    "env": {"type": "boolean", "description": "when true on get, return an export token for use in a shell command"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let dir = self.secrets_dir();
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("list").to_string();
            let key = args.get("key").and_then(Value::as_str).unwrap_or("").to_string();
            let value = args.get("value").and_then(Value::as_str).unwrap_or("").to_string();
            let want_env = args.get("env").and_then(Value::as_bool).unwrap_or(false);

            fs::create_dir_all(&dir).await.ok();

            let out = match action.as_str() {
                "set" => {
                    if key.is_empty() {
                        "usage: secrets set <key> <value>".to_string()
                    } else if value.is_empty() {
                        format!("refusing to store empty value for {key:?}")
                    } else {
                        let path = dir.join(&key);
                        let masked = mask_value(&value);
                        // Write via temp file + rename so the secret never sits
                        // half-written, then chmod 600.
                        let tmp = dir.join(format!(".{key}.tmp"));
                        if let Err(e) = fs::write(&tmp, &value).await {
                            format!("failed to write secret: {e:#}")
                        } else {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await;
                            }
                            let _ = fs::rename(&tmp, &path).await;
                            format!("stored secret `{key}` ({masked})")
                        }
                    }
                }
                "get" => {
                    let path = dir.join(&key);
                    match fs::read_to_string(&path).await {
                        Ok(v) => {
                            if want_env {
                                // Sanitize key to a valid-ish env var name.
                                let env_key = sanitize_env_name(&key);
                                let sh = shell_quote(&v);
                                format!("<cmd>export {env_key}={sh}; # resolves to `{key}`</cmd>")
                            } else {
                                v
                            }
                        }
                        Err(_) => format!("no secret named `{key}` — set it first (secrets set {key} <value>)"),
                    }
                }
                "list" => {
                    let mut names: Vec<String> = Vec::new();
                    if let Ok(mut rd) = fs::read_dir(&dir).await {
                        while let Ok(Some(e)) = rd.next_entry().await {
                            let name = e.file_name().to_string_lossy().to_string();
                            if !name.starts_with('.') {
                                names.push(name);
                            }
                        }
                    }
                    names.sort();
                    if names.is_empty() {
                        "no secrets stored".to_string()
                    } else {
                        format!("stored secrets:\n  {}", names.join("\n  "))
                    }
                }
                "delete" => {
                    let path = dir.join(&key);
                    match fs::remove_file(&path).await {
                        Ok(_) => format!("deleted secret `{key}`"),
                        Err(_) => format!("no secret named `{key}`"),
                    }
                }
                other => format!("unknown action {other:?} — use set|get|list|delete"),
            };
            ok_outcome("", "secrets", out, started.elapsed().as_millis() as u64)
        })
    }
}

/// Mask most of the value for the confirm message so it's not echoed verbatim.
fn mask_value(v: &str) -> String {
    if v.len() <= 4 {
        "••••".to_string()
    } else {
        let tail = &v[v.len() - 4..];
        format!("••••{tail}")
    }
}

fn sanitize_env_name(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    for (i, c) in k.chars().enumerate() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if i > 0 && c == '-' {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('S');
    }
    if out.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        out.insert(0, 'S');
    }
    out
}

/// Minimal POSIX single-quote shell escaping.
fn shell_quote(v: &str) -> String {
    if v.chars().all(|c| c.is_ascii_alphanumeric() || "_/.-".contains(c)) {
        v.to_string()
    } else {
        format!("'{}'", v.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_short_and_long() {
        assert_eq!(mask_value("ab"), "••••");
        assert_eq!(mask_value("abcdefgh"), "••••efgh");
    }

    #[test]
    fn env_name_sanitized() {
        assert_eq!(sanitize_env_name("api-key"), "api_key");
        assert_eq!(sanitize_env_name("9lives"), "S9lives");
    }

    #[test]
    fn shell_quote_spaces_and_quotes() {
        assert_eq!(shell_quote("abc"), "abc");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}