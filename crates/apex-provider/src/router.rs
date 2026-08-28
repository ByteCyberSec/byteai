//! Router (Phase 8): capability-based model routing + learning.
//!
//! Task classes → required capabilities → model ranking. Routes:
//!   - `fast`     (trivial answers, classification)  → fastest model
//!   - `code`     (editing, tool-heavy)              → strong tool-calling
//!   - `reasoning`(hard problems)                    → reasoning model
//!   - `memory`   (recall, summarization)            → balanced
//!     Learns from per-(model,class) success stats stored in the memory DB.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskClass {
    Fast,
    Code,
    Reasoning,
    Memory,
    Default,
}

impl TaskClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskClass::Fast => "fast",
            TaskClass::Code => "code",
            TaskClass::Reasoning => "reasoning",
            TaskClass::Memory => "memory",
            TaskClass::Default => "default",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> TaskClass {
        match s {
            "fast" => TaskClass::Fast,
            "code" => TaskClass::Code,
            "reasoning" => TaskClass::Reasoning,
            "memory" => TaskClass::Memory,
            _ => TaskClass::Default,
        }
    }
}

/// Heuristic: classify a task prompt into a class. Cheap string signals.
pub fn classify(prompt: &str) -> TaskClass {
    let p = prompt.to_lowercase();
    let reasoning = ["why", "explain", "prove", "design", "architecture", "review", "analyze", "compare", "trade-off", "optimize"];
    let code = ["implement", "refactor", "fix", "bug", "test", "cargo", "npm", "compile", "edit", "write code", "patch", "function", "api"];
    let memory = ["recall", "remember", "what did", "summarize", "search", "notes", "memory", "previous session"];
    let fast = ["yes/no", "reply with exactly", "summarize in one", "what is 2", "hi", "hello"];

    let count = |words: &[&str]| words.iter().filter(|w| p.contains(**w)).count();
    let (r, c, m, f) = (count(&reasoning), count(&code), count(&memory), count(&fast));
    if f > 0 && r == 0 && c == 0 {
        TaskClass::Fast
    } else if r > c && r > 0 {
        TaskClass::Reasoning
    } else if c > 0 {
        TaskClass::Code
    } else if m > 0 {
        TaskClass::Memory
    } else {
        TaskClass::Default
    }
}

#[derive(Debug, Clone)]
pub struct ModelProfile {
    pub name: String,
    pub provider: String,
    /// Capabilities this model satisfies.
    pub classes: Vec<TaskClass>,
    /// Latency penalty (relative, lower = faster).
    pub latency: f64,
}

impl ModelProfile {
    pub fn new(name: &str, provider: &str, classes: &[TaskClass], latency: f64) -> Self {
        Self { name: name.into(), provider: provider.into(), classes: classes.to_vec(), latency }
    }
}

pub struct Router {
    pub models: Vec<ModelProfile>,
    pub stats: HashMap<(String, String), (u32, u32)>, // (model,class) -> (success, total)
}

impl Router {
    pub fn new(models: Vec<ModelProfile>) -> Self {
        Self { models, stats: HashMap::new() }
    }

    /// Pick the best model for a task class, breaking ties by learned
    /// success rate, then by latency.
    pub fn route(&self, class: TaskClass) -> Option<&ModelProfile> {
        let mut best: Option<&ModelProfile> = None;
        let mut best_score = f64::MIN;
        for m in &self.models {
            if !m.classes.contains(&class) && class != TaskClass::Default {
                continue;
            }
            let base = if class == TaskClass::Default { -m.latency } else { 0.0 - m.latency };
            let learned = self.success_rate(&m.name, class.as_str()) * 2.0;
            let score = base + learned;
            if score > best_score {
                best_score = score;
                best = Some(m);
            }
        }
        best
    }

    pub fn success_rate(&self, model: &str, class: &str) -> f64 {
        self.stats.get(&(model.to_string(), class.to_string()))
            .map(|(ok, total)| if *total == 0 { 0.5 } else { *ok as f64 / *total as f64 })
            .unwrap_or(0.5)
    }

    pub fn record(&mut self, model: &str, class: TaskClass, success: bool) {
        let key = (model.to_string(), class.as_str().to_string());
        let e = self.stats.entry(key).or_insert((0, 0));
        e.1 += 1;
        if success {
            e.0 += 1;
        }
    }

    /// Render routing table for doctor/status.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for class in [TaskClass::Fast, TaskClass::Code, TaskClass::Reasoning, TaskClass::Memory, TaskClass::Default] {
            if let Some(m) = self.route(class) {
                let rate = self.success_rate(&m.name, class.as_str());
                out.push_str(&format!("  {:<10} → {} ({}@{:.0}%)\n", class.as_str(), m.name, m.provider, rate * 100.0));
            } else {
                out.push_str(&format!("  {:<10} → (no model)\n", class.as_str()));
            }
        }
        out
    }
}

impl Default for Router {
    fn default() -> Self {
        let models = vec![
            ModelProfile::new("deepseek-v4-flash", "b.ai", &[TaskClass::Fast, TaskClass::Code, TaskClass::Memory, TaskClass::Default], 1.0),
            ModelProfile::new("deepseek-reasoner", "b.ai", &[TaskClass::Reasoning, TaskClass::Code], 3.0),
        ];
        Self::new(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basics() {
        assert_eq!(classify("reply with exactly: hello"), TaskClass::Fast);
        assert_eq!(classify("implement the edit function in Rust"), TaskClass::Code);
        assert_eq!(classify("explain the architecture trade-offs and prove why"), TaskClass::Reasoning);
        assert_eq!(classify("recall what we did in the previous session"), TaskClass::Memory);
        assert_eq!(classify("how are you today"), TaskClass::Default);
    }

    #[test]
    fn routing_picks_reasoning_for_hard() {
        let r = Router::default();
        let m = r.route(TaskClass::Reasoning).unwrap();
        assert_eq!(m.name, "deepseek-reasoner");
        let m = r.route(TaskClass::Fast).unwrap();
        assert_eq!(m.name, "deepseek-v4-flash");
    }

    #[test]
    fn learning_biases_route() {
        let mut r = Router::default();
        // Repeated failures on fast tasks with flash should not switch (only model),
        // but verify stats accumulate.
        for _ in 0..10 {
            r.record("deepseek-v4-flash", TaskClass::Fast, false);
        }
        assert_eq!(r.success_rate("deepseek-v4-flash", "fast"), 0.0);
    }
}
