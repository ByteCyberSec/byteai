//! ProviderPool — multi-provider failover pool.
//!
//! Holds an ordered list of (name, Client, model) tuples. The active provider
//! is the current index; on hard failure the pool rotates to the next entry so
//! the agent survives a single-provider outage without losing the task.
//!
//! Hermes parity: credential pools + rotation. Without this, a transient
//! provider outage (rate-limit that exhausts the 4 retries, or a 5xx that
//! lasts longer than the retry window) kills the entire multi-turn task.

use crate::Client;

/// A single provider entry in the pool.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub name: String,
    pub client: Client,
    pub model: String,
}

/// Ordered provider pool with automatic failover rotation.
///
/// Construction: `ProviderPool::new(vec![
///   ("bai", client_bai, "deepseek-v4-flash"),
///   ("omniroute", client_omni, "deepseek-v4-flash"),
/// ])`
///
/// On every call to `current()` the pool returns the active entry. After a
/// provider is marked as failed (via `report_failure`), `rotate()` advances
/// to the next functional entry. The pool remembers which providers have
/// failed during this turn so it doesn't retry them.
#[derive(Debug, Clone)]
pub struct ProviderPool {
    entries: Vec<ProviderEntry>,
    active: usize,
    /// Set of provider indices that failed this turn (never retry in the same
    /// turn; after a successful call, the failure set is reset).
    failed: Vec<usize>,
}

impl ProviderPool {
    /// Build a pool from at least one entry. The first entry is the active one.
    pub fn new(entries: Vec<ProviderEntry>) -> Self {
        assert!(!entries.is_empty(), "ProviderPool needs at least one entry");
        Self {
            entries,
            active: 0,
            failed: Vec::new(),
        }
    }

    /// Build a fallback pool from a single entry (no failover capability).
    /// Used by tests and minimal configs.
    pub fn single(name: &str, client: Client, model: &str) -> Self {
        Self::new(vec![ProviderEntry {
            name: name.to_string(),
            client,
            model: model.to_string(),
        }])
    }

    /// The active provider entry.
    pub fn current(&self) -> &ProviderEntry {
        &self.entries[self.active]
    }

    /// The active provider's client.
    pub fn client(&self) -> &Client {
        &self.entries[self.active].client
    }

    /// The active provider's model.
    pub fn model(&self) -> &str {
        &self.entries[self.active].model
    }

    /// The active provider's name.
    pub fn name(&self) -> &str {
        &self.entries[self.active].name
    }

    /// Report a failure on the active provider. Marks it as failed and
    /// rotates to the next viable entry. Returns true if a rotation happened
    /// (there is still at least one non-failed provider to try), false if
    /// every provider in the pool has failed (all dead).
    pub fn report_failure(&mut self) -> bool {
        let idx = self.active;
        if !self.failed.contains(&idx) {
            self.failed.push(idx);
        }
        // If every provider has failed, no failover is possible.
        if self.failed.len() >= self.entries.len() {
            return false;
        }
        // Rotate to the next non-failed entry.
        for _ in 0..self.entries.len() {
            self.active = (self.active + 1) % self.entries.len();
            if !self.failed.contains(&self.active) {
                return true;
            }
        }
        false
    }

    /// Mark the current provider as successful — reset the failure set so
    /// a future failure can try the full pool again.
    pub fn report_success(&mut self) {
        self.failed.clear();
    }

    /// Replace the ACTIVE provider's client + model in place (runtime
    /// provider switch — TUI `/provider`, REPL `/provider <name>`). Keeps the
    /// pool structure; only the active entry's transport/model change.
    pub fn replace_active(&mut self, name: &str, client: Client, model: &str) {
        let entry = &mut self.entries[self.active];
        entry.name = name.to_string();
        entry.client = client;
        entry.model = model.to_string();
        // A fresh transport resets any accumulated failure state.
        self.failed.clear();
    }

    /// How many providers are in the pool.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Describe the pool (for doctor / status).
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for (i, e) in self.entries.iter().enumerate() {
            let marker = if i == self.active { " ▸" } else { "  " };
            let failed = if self.failed.contains(&i) { " (FAILED)" } else { "" };
            out.push_str(&format!("{marker} {}{failed}\n", e.name));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Client;

    fn test_client() -> Client {
        Client::new("http://127.0.0.1:1/v1", "test").unwrap()
    }

    #[test]
    fn pool_starts_at_first_entry() {
        let pool = ProviderPool::new(vec![
            ProviderEntry { name: "a".into(), client: test_client(), model: "m1".into() },
            ProviderEntry { name: "b".into(), client: test_client(), model: "m2".into() },
        ]);
        assert_eq!(pool.current().name, "a");
        assert_eq!(pool.current().model, "m1");
    }

    #[test]
    fn rotation_fails_over_to_next() {
        let mut pool = ProviderPool::new(vec![
            ProviderEntry { name: "a".into(), client: test_client(), model: "m1".into() },
            ProviderEntry { name: "b".into(), client: test_client(), model: "m2".into() },
            ProviderEntry { name: "c".into(), client: test_client(), model: "m3".into() },
        ]);
        assert!(pool.report_failure());
        assert_eq!(pool.current().name, "b");
        assert!(pool.report_failure());
        assert_eq!(pool.current().name, "c");
        // All three now failed — the 3rd failure returns false (nothing left).
        assert!(!pool.report_failure());
    }

    #[test]
    fn success_resets_failure_set() {
        let mut pool = ProviderPool::new(vec![
            ProviderEntry { name: "a".into(), client: test_client(), model: "m1".into() },
            ProviderEntry { name: "b".into(), client: test_client(), model: "m2".into() },
        ]);
        pool.report_failure(); // move to b
        assert_eq!(pool.current().name, "b");
        pool.report_success(); // reset — a is un-failed
        // Now rotate back to a (it's the only non-failed after reset).
        assert!(pool.report_failure());
        assert_eq!(pool.current().name, "a");
    }

    #[test]
    fn single_entry_returns_false_on_rotation() {
        let mut pool = ProviderPool::single("only", test_client(), "m1");
        // Can't rotate — only one provider.
        assert!(!pool.report_failure());
        assert_eq!(pool.current().name, "only");
    }
}