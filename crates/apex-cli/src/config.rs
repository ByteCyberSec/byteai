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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentSection {
    #[serde(default)]
    pub model: String,
    /// Preferred provider name (matches [[providers]] name). When set, the
    /// TUI/REPL uses this provider instead of the first key-bearing entry.
    #[serde(default)]
    pub default_provider: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

fn default_max_iterations() -> u32 {
    20
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
            agent: AgentSection { model: "deepseek-v4-flash".into(), default_provider: String::new(), max_iterations: 20 },
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
