//! Configuration: TOML config + env overrides + provider resolution.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
}

fn default_max_iterations() -> u32 { 150 }
fn default_delegation_max_iterations() -> Option<u32> { Some(250) }
fn default_warn_ratio() -> f32 { 0.8 }
fn default_true() -> bool { true }
fn default_auto_min_tools() -> u32 { 5 }
fn default_auto_min_iters() -> u32 { 10 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    #[serde(default)]
    pub model: String,
    /// Preferred provider name (matches [[providers]] name). When set, the
    /// TUI/REPL uses this provider instead of the first key-bearing entry.
    #[serde(default)]
    pub default_provider: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Optional wall-clock budget per user turn (seconds). 0 or absent = off.
    #[serde(default)]
    pub run_budget_seconds: Option<u64>,
    /// Per-tool execution timeout in seconds. Default 300.
    #[serde(default)]
    pub tool_timeout_seconds: Option<u64>,
    /// Delegation budget: max iterations each spawned subagent gets.
    #[serde(default = "default_delegation_max_iterations")]
    pub delegation_max_iterations: Option<u32>,
    /// Fraction of the iteration cap at which a proactive "begin wrapping up"
    /// notice is injected into the model context (0.8 = 80%).
    #[serde(default = "default_warn_ratio")]
    pub budget_warn_ratio: f32,
    /// Auto-run a non-blocking self-review + lesson recording after heavy
    /// turns (many tool calls or iterations).
    #[serde(default = "default_true")]
    pub auto_review_enabled: bool,
    /// Minimum tool calls in a turn to trigger auto-review.
    #[serde(default = "default_auto_min_tools")]
    pub auto_review_min_tools: u32,
    /// Minimum iterations in a turn to trigger auto-review.
    #[serde(default = "default_auto_min_iters")]
    pub auto_review_min_iters: u32,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            model: String::new(),
            default_provider: String::new(),
            max_iterations: default_max_iterations(),
            run_budget_seconds: None,
            tool_timeout_seconds: None,
            delegation_max_iterations: default_delegation_max_iterations(),
            budget_warn_ratio: default_warn_ratio(),
            auto_review_enabled: default_true(),
            auto_review_min_tools: default_auto_min_tools(),
            auto_review_min_iters: default_auto_min_iters(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderEntry {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// Name of an environment variable holding the key (preferred).
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub model: String,
}

impl ProviderEntry {
    pub fn resolved_key(&self) -> String {
        if !self.api_key_env.is_empty() {
            if let Ok(v) = std::env::var(&self.api_key_env) {
                if !v.is_empty() {
                    return v;
                }
            }
        }
        self.api_key.clone()
    }
}

impl Default for Config {
    fn default() -> Self {
        // Sensible local-first defaults: OmniRoute + any OpenAI-compatible env.
        Self {
            agent: AgentSection {
                model: "deepseek-v4-flash".into(),
                default_provider: String::new(),
                max_iterations: default_max_iterations(),
                ..AgentSection::default()
            },
            providers: vec![
                ProviderEntry {
                    name: "omniroute".into(),
                    base_url: "http://localhost:20128/v1".into(),
                    api_key: String::new(),
                    api_key_env: "HERMES_CUSTOM_LOCALHOST_20128_API_KEY".into(),
                    model: String::new(),
                },
                ProviderEntry {
                    name: "bai".into(),
                    base_url: "https://api.b.ai/v1".into(),
                    api_key: String::new(),
                    api_key_env: "HERMES_CUSTOM_API_B_AI_API_KEY".into(),
                    model: "deepseek-v4-flash".into(),
                },
            ],
        }
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var("BYTEAI_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config")).join("byteai"))
}

pub fn data_dir() -> PathBuf {
    std::env::var("BYTEAI_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("~/.local/share")).join("byteai"))
}

pub fn load() -> Result<Config> {
    let path = config_dir().join("config.toml");
    if path.exists() {
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(cfg)
    } else {
        Ok(Config::default())
    }
}

/// Resolve the effective provider entry, honoring CLI flags and env overrides.
pub fn resolve_provider(cfg: &Config, cli_provider: Option<&str>, cli_base_url: Option<&str>, cli_key: Option<&str>) -> ProviderEntry {
    let env_name = std::env::var("BYTEAI_PROVIDER").ok();
    let env_url = std::env::var("BYTEAI_BASE_URL").ok();
    let env_key = std::env::var("BYTEAI_API_KEY").ok();

    // Explicit flags win.
    let name = cli_provider.or(env_name.as_deref());
    if let Some(n) = name {
        if let Some(p) = cfg.providers.iter().find(|p| p.name == n) {
            let mut p = p.clone();
            if let Some(u) = cli_base_url.or(env_url.as_deref()) {
                p.base_url = u.into();
            }
            if let Some(k) = cli_key.or(env_key.as_deref()) {
                p.api_key = k.into();
                p.api_key_env = String::new();
            }
            return p;
        }
    }
    // Explicit base_url without a named provider.
    if let Some(u) = cli_base_url.or(env_url.as_deref()) {
        return ProviderEntry {
            name: "custom".into(),
            base_url: u.into(),
            api_key: cli_key.or(env_key.as_deref()).unwrap_or("").into(),
            api_key_env: String::new(),
            model: String::new(),
        };
    }
    // Config default_provider wins over "first with key" so the configured
    // model and provider stay matched (e.g. oc/mimo-v2.5-free on omniroute).
    if !cfg.agent.default_provider.is_empty() {
        if let Some(p) = cfg.providers.iter().find(|p| p.name == cfg.agent.default_provider) {
            return p.clone();
        }
    }
    // First provider with a resolved key, else the first entry.
    cfg.providers
        .iter()
        .find(|p| !p.resolved_key().is_empty())
        .cloned()
        .or_else(|| cfg.providers.first().cloned())
        .unwrap_or_default()
}

/// Effective model: CLI > env > config > provider default > builtin.
pub fn resolve_model(cfg: &Config, cli_model: Option<&str>, provider: &ProviderEntry) -> String {
    cli_model
        .map(String::from)
        .or_else(|| std::env::var("BYTEAI_MODEL").ok().filter(|s| !s.is_empty()))
        .or_else(|| if cfg.agent.model.is_empty() { None } else { Some(cfg.agent.model.clone()) })
        .or_else(|| if provider.model.is_empty() { None } else { Some(provider.model.clone()) })
        .unwrap_or_else(|| "deepseek-v4-flash".into())
}

/// Write the config back to config.toml (creating the dir/file if needed).
/// Preserves every existing field, including inline API keys — the whole
/// struct is round-tripped, so nothing configured is lost.
pub fn save(cfg: &Config) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let text = toml::to_string_pretty(cfg).context("serialize config")?;
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))
}

/// Persist a model switch: updates [agent] model AND the active provider's
/// model so the choice survives restart and stays matched on provider switch.
pub fn set_model(cfg: &mut Config, provider_name: &str, model: &str) -> Result<()> {
    cfg.agent.model = model.to_string();
    if let Some(p) = cfg.providers.iter_mut().find(|p| p.name == provider_name) {
        p.model = model.to_string();
    }
    save(cfg)
}

/// Persist a default-provider switch.
pub fn set_default_provider(cfg: &mut Config, provider_name: &str) -> Result<()> {
    cfg.agent.default_provider = provider_name.to_string();
    save(cfg)
}

/// Add a brand-new provider (+ optional default model) to the config and
/// make it the default provider. Returns an error if the name already exists.
/// `api_key` and `api_key_env` are mutually exclusive — the non-empty one
/// wins; if both empty, the entry stores no key (env override still works).
pub fn add_provider(
    cfg: &mut Config,
    name: &str,
    base_url: &str,
    api_key: &str,
    api_key_env: &str,
    model: &str,
) -> Result<()> {
    if cfg.providers.iter().any(|p| p.name == name) {
        anyhow::bail!("provider '{name}' already exists — pick another name or edit config.toml");
    }
    let (key, env) = if !api_key.is_empty() {
        (api_key.to_string(), String::new())
    } else {
        (String::new(), api_key_env.to_string())
    };
    cfg.providers.push(ProviderEntry {
        name: name.to_string(),
        base_url: base_url.to_string(),
        api_key: key,
        api_key_env: env,
        model: model.to_string(),
    });
    cfg.agent.default_provider = name.to_string();
    if !model.is_empty() {
        cfg.agent.model = model.to_string();
    }
    save(cfg)
}

/// Names of every provider in the config (for the palette / provider list).
pub fn provider_names(cfg: &Config) -> Vec<String> {
    cfg.providers.iter().map(|p| p.name.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// Tests mutate the process-global BYTEAI_CONFIG_DIR env var, so they
    /// must run one at a time (parallel threads would race on `load()`).
    static CFG_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        CFG_LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("byteai-config-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_roundtrips_all_fields_including_key() {
        let _g = lock();
        let dir = temp_dir("roundtrip");
        unsafe { std::env::set_var("BYTEAI_CONFIG_DIR", &dir) };
        let mut cfg = Config::default();
        cfg.agent.model = "m1".into();
        cfg.providers[0].api_key = "key-value-abc-123".into(); // must survive round-trip
        save(&cfg).unwrap();
        let back = load().unwrap();
        assert_eq!(back.agent.model, "m1");
        assert_eq!(back.providers[0].api_key, "key-value-abc-123");
        assert_eq!(back.providers.len(), 2);
    }

    #[test]
    fn set_model_persists_agent_and_provider() {
        let _g = lock();
        let dir = temp_dir("setmodel");
        unsafe { std::env::set_var("BYTEAI_CONFIG_DIR", &dir) };
        let mut cfg = Config::default();
        set_model(&mut cfg, "bai", "gpt-x").unwrap();
        let back = load().unwrap();
        assert_eq!(back.agent.model, "gpt-x");
        assert_eq!(back.providers.iter().find(|p| p.name == "bai").unwrap().model, "gpt-x");
        assert_eq!(back.providers.iter().find(|p| p.name == "omniroute").unwrap().model, "");
    }

    #[test]
    fn add_provider_appends_and_becomes_default() {
        let _g = lock();
        let dir = temp_dir("addprov");
        unsafe { std::env::set_var("BYTEAI_CONFIG_DIR", &dir) };
        let mut cfg = Config::default();
        add_provider(&mut cfg, "groq", "https://api.groq.com/v1", "", "GROQ_KEY", "llama-3").unwrap();
        let back = load().unwrap();
        assert_eq!(back.providers.len(), 3);
        let groq = back.providers.iter().find(|p| p.name == "groq").unwrap();
        assert_eq!(groq.base_url, "https://api.groq.com/v1");
        assert_eq!(groq.api_key_env, "GROQ_KEY");
        assert_eq!(groq.model, "llama-3");
        assert_eq!(back.agent.default_provider, "groq");
        assert_eq!(back.agent.model, "llama-3");
    }

    #[test]
    fn add_provider_rejects_duplicate_and_accepts_literal_key() {
        let _g = lock();
        let dir = temp_dir("dup");
        unsafe { std::env::set_var("BYTEAI_CONFIG_DIR", &dir) };
        let mut cfg = Config::default();
        add_provider(&mut cfg, "dup", "http://x/v1", "lit-key-xyz", "", "m").unwrap();
        let err = add_provider(&mut cfg, "dup", "http://x/v1", "", "", "m2").unwrap_err();
        assert!(err.to_string().contains("already exists"));
        let back = load().unwrap();
        let dup = back.providers.iter().find(|p| p.name == "dup").unwrap();
        assert_eq!(dup.api_key, "lit-key-xyz"); // literal key stored, not env
        assert_eq!(dup.api_key_env, "");
        assert_eq!(back.agent.model, "m"); // duplicate didn't clobber model
    }

    #[test]
    fn set_default_provider_persists() {
        let _g = lock();
        let dir = temp_dir("defprov");
        unsafe { std::env::set_var("BYTEAI_CONFIG_DIR", &dir) };
        let mut cfg = Config::default();
        set_default_provider(&mut cfg, "bai").unwrap();
        assert_eq!(load().unwrap().agent.default_provider, "bai");
    }
}
