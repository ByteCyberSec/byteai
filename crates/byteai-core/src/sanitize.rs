//! Message sanitization and secret redaction (Hermes parity).
//!
//! Before messages are sent to the provider, this module:
//! 1. Redacts known secrets (API keys, env vars with KEY/TOKEN/SECRET names)
//!    from tool outputs and message content.
//! 2. Sanitizes malformed content (null bytes, lone surrogates) that
//!    providers reject.
//!
//! Without this, tool output containing secrets (e.g. `shell env` or `config`
//! reads) leaks into the provider's context window and persists in history.

use byteai_types::Message;

/// Redact known secrets and sanitize message content before sending to the
/// provider. Operates on a clone of the history so the original is preserved
/// (the redacted version is only used for the wire call).
pub fn sanitize(messages: &[Message]) -> Vec<Message> {
    let secrets = collect_secrets();
    messages
        .iter()
        .map(|m| {
            let mut m2 = m.clone();
            if let Some(c) = &mut m2.content {
                *c = redact(c, &secrets);
                *c = sanitize_text(c);
            }
            if let Some(r) = &mut m2.reasoning {
                *r = redact(r, &secrets);
                *r = sanitize_text(r);
            }
            m2
        })
        .collect()
}

/// Collect known secret strings from the environment.
fn collect_secrets() -> Vec<String> {
    let mut secrets: Vec<String> = Vec::new();
    // Scan env vars for KEY/TOKEN/SECRET/PASSWORD patterns.
    for (key, val) in std::env::vars() {
        let upper = key.to_uppercase();
        if upper.contains("KEY") || upper.contains("TOKEN") || upper.contains("SECRET")
            || upper.contains("PASSWORD") || upper.contains("API") || upper.contains("AUTH")
            || upper == "BYTEAI_API_KEY" || upper.starts_with("HERMES_CUSTOM")
        {
            if !val.is_empty() && val.len() >= 8 {
                secrets.push(val);
            }
        }
    }
    // Also scan for common config file path env vars.
    for var in &["BYTEAI_API_KEY", "BYTEAI_BASE_URL", "HOME", "USER"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && v.len() >= 8 {
                secrets.push(v);
            }
        }
    }
    secrets
}

/// Replace all occurrences of known secret strings in text with [REDACTED].
fn redact(text: &str, secrets: &[String]) -> String {
    let mut result = text.to_string();
    for secret in secrets {
        // Only replace if the secret is a reasonable length (avoid false
        // positives on short values like "true" or "1").
        if secret.len() < 8 {
            continue;
        }
        // Use case-insensitive matching for short secrets to catch
        // lowercased versions.
        if secret.len() < 20 {
            // Blind replacement for exact case
            result = result.replace(secret, "[REDACTED]");
            // Also try lowercase
            result = result.replace(&secret.to_lowercase(), "[REDACTED]");
        } else {
            result = result.replace(secret, "[REDACTED]");
        }
    }
    result
}

/// Strip null bytes and other problematic characters that providers reject.
/// Note: Rust `str` is always valid UTF-8, so lone surrogates cannot exist;
/// we still guard against noncharacters (U+FFFE/U+FFFF) and null bytes.
fn sanitize_text(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            c != '\0'              // null byte
                && c != '\u{FFFE}' // noncharacter
                && c != '\u{FFFF}' // noncharacter
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_api_key_from_text() {
        let secrets = vec!["sk-abc123".to_string(), "really-long-api-key-xyz".to_string()];
        let text = "my key is sk-abc123 and also really-long-api-key-xyz here";
        assert_eq!(redact(text, &secrets), "my key is [REDACTED] and also [REDACTED] here");
    }

    #[test]
    fn redact_ignores_short_secrets() {
        let secrets = vec!["short".to_string()];
        assert_eq!(redact("this is short", &secrets), "this is short");
    }

    #[test]
    fn sanitize_removes_null_bytes() {
        let text = "hello\0world";
        assert_eq!(sanitize_text(text), "helloworld");
    }

    #[test]
    fn sanitize_removes_noncharacters() {
        let text = format!("hello\u{FFFE}world\u{FFFF}!");
        assert_eq!(sanitize_text(&text), "helloworld!");
    }

    #[test]
    fn sanitize_passes_clean_text() {
        let text = "hello world, how are you?";
        assert_eq!(sanitize_text(text), text);
    }
}