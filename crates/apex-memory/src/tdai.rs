//! TencentDB Agent Memory client — a reqwest-based HTTP client for the
//! MemoryCore REST API (port 8420). Provides L0 capture, L1/L2/L3 recall,
//! skill search, and session management for the byteai agent.
//!
//! API docs: vendor/TencentDB-Agent-Memory/MemoryCore/v3-api-memorycore-doc.md

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// TDAI client configuration.
#[derive(Debug, Clone)]
pub struct TdaiConfig {
    /// MemoryCore base URL (e.g. "http://127.0.0.1:8420").
    pub base_url: String,
    /// Gateway API key (Authorization: Bearer). Use "local" for default.
    pub gateway_api_key: String,
    /// Instance / service ID (x-tdai-service-id). Default "default".
    pub service_id: String,
    /// Business user key (x-tdai-user-key).
    pub user_key: String,
    /// Team ID for memory isolation.
    pub team_id: String,
    /// Agent ID for memory isolation.
    pub agent_id: String,
    /// User ID for memory isolation.
    pub user_id: String,
    /// Optional task ID.
    pub task_id: Option<String>,
    /// Session / conversation ID for L0 capture.
    pub conversation_id: String,
    /// Whether the client is enabled (short-circuits if false).
    pub enabled: bool,
}

impl Default for TdaiConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8420".into(),
            gateway_api_key: "local".into(),
            service_id: "default".into(),
            user_key: String::new(),
            team_id: String::new(),
            agent_id: String::new(),
            user_id: String::new(),
            task_id: None,
            conversation_id: String::new(),
            enabled: false,
        }
    }
}

/// TDAI API response envelope.
#[derive(Debug, Deserialize)]
pub struct TdaiResponse<T> {
    pub code: i64,
    pub message: String,
    pub request_id: Option<String>,
    pub data: Option<T>,
}

/// Auth/verify response data.
#[derive(Debug, Deserialize)]
pub struct AuthData {
    pub valid: bool,
    pub user: Option<Value>,
}

/// Conversation add response.
#[derive(Debug, Deserialize)]
pub struct ConversationAddData {
    pub accepted_ids: Vec<String>,
    pub total_count: i64,
}

/// Conversation query item.
#[derive(Debug, Deserialize, Serialize)]
pub struct ConversationItem {
    pub id: Option<String>,
    pub role: String,
    pub content: String,
    pub session_id: Option<String>,
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub timestamp: Option<String>,
    pub recorded_at: Option<String>,
    pub score: Option<f64>,
}

/// Atomic memory item (L1).
#[derive(Debug, Deserialize)]
pub struct AtomicItem {
    pub id: String,
    #[serde(default)]
    pub version: serde_json::Value,
    #[serde(rename = "type")]
    pub mem_type: Option<String>,
    pub content: String,
    pub background: Option<String>,
    pub team_id: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub score: Option<f64>,
}

/// Scenario entry (L2).
#[derive(Debug, Deserialize)]
pub struct ScenarioEntry {
    pub path: String,
    pub summary: Option<String>,
    pub version: i64,
}

/// Scenario read data (L2).
#[derive(Debug, Deserialize)]
pub struct ScenarioReadData {
    pub path: String,
    pub version: Option<i64>,
    pub content: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Core read data (L3).
#[derive(Debug, Deserialize)]
pub struct CoreReadData {
    pub content: Option<String>,
    pub version: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Skill summary.
#[derive(Debug, Deserialize)]
pub struct SkillSummary {
    pub skill_id: Option<String>,
    pub name: String,
    pub version: i64,
    pub status: Option<String>,
    pub owner_user_id: Option<String>,
    pub owner_agent_id: Option<String>,
    pub team_id: Option<String>,
    pub content: Option<String>,
    pub score: Option<f64>,
    pub snippet: Option<String>,
}

/// HTTP client wrapper for the MemoryCore API.
#[derive(Debug, Clone)]
pub struct TdaiClient {
    client: reqwest::Client,
    config: TdaiConfig,
}

impl TdaiClient {
    /// Create a new client from config.
    pub fn new(config: TdaiConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self { client, config }
    }

    /// Build the standard headers for data plane / meta API calls.
    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("Content-Type", "application/json".parse().unwrap());
        h.insert("x-tdai-service-id", self.config.service_id.parse().unwrap());
        h.insert("Authorization", format!("Bearer {}", self.config.gateway_api_key).parse().unwrap());
        if !self.config.user_key.is_empty() {
            h.insert("x-tdai-user-key", self.config.user_key.parse().unwrap());
        }
        h
    }

    /// Health check.
    pub async fn health(&self) -> Result<String, String> {
        let resp = self.client
            .get(format!("{}/health", self.config.base_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("health request failed: {e}"))?;
        let body = resp.text().await.map_err(|e| format!("health read failed: {e}"))?;
        Ok(body)
    }

    /// Verify a user key.
    pub async fn auth_verify(&self, user_key: &str) -> Result<AuthData, String> {
        let resp: TdaiResponse<AuthData> = self.post("/v3/meta/auth/verify", &serde_json::json!({"user_key": user_key})).await?;
        resp.data.ok_or_else(|| format!("auth/verify failed: {}", resp.message))
    }

    /// Capture L0 conversation messages.
    pub async fn conversation_add(&self, session_id: &str, messages: Vec<Value>) -> Result<ConversationAddData, String> {
        let mut body = serde_json::json!({
            "session_id": session_id,
            "messages": messages,
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        if let Some(ref t) = self.config.task_id {
            body.as_object_mut().unwrap().insert("task_id".into(), serde_json::json!(t));
        }
        let resp: TdaiResponse<ConversationAddData> = self.post("/v3/conversation/add", &body).await?;
        resp.data.ok_or_else(|| format!("conversation/add failed: {}", resp.message))
    }

    /// Query L0 conversation messages.
    pub async fn conversation_query(&self, session_id: &str, limit: i64) -> Result<Vec<ConversationItem>, String> {
        let mut body = serde_json::json!({
            "session_id": session_id,
            "limit": limit,
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        if let Some(ref t) = self.config.task_id {
            body.as_object_mut().unwrap().insert("task_id".into(), serde_json::json!(t));
        }
        let resp: TdaiResponse<Value> = self.post("/v3/conversation/query", &body).await?;
        let data = resp.data.ok_or_else(|| format!("conversation/query failed: {}", resp.message))?;
        let items = data.get("messages").and_then(|m| serde_json::from_value::<Vec<ConversationItem>>(m.clone()).ok()).unwrap_or_default();
        Ok(items)
    }

    /// Search L0 conversation messages.
    pub async fn conversation_search(&self, query: &str, limit: i64) -> Result<Vec<ConversationItem>, String> {
        let mut body = serde_json::json!({
            "query": query,
            "limit": limit,
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        if let Some(ref t) = self.config.task_id {
            body.as_object_mut().unwrap().insert("task_id".into(), serde_json::json!(t));
        }
        let resp: TdaiResponse<Value> = self.post("/v3/conversation/search", &body).await?;
        let data = resp.data.ok_or_else(|| format!("conversation/search failed: {}", resp.message))?;
        let items = data.get("messages").and_then(|m| serde_json::from_value::<Vec<ConversationItem>>(m.clone()).ok()).unwrap_or_default();
        Ok(items)
    }

    /// Search L1 atomic memories.
    pub async fn atomic_search(&self, query: &str, limit: i64) -> Result<Vec<AtomicItem>, String> {
        let body = serde_json::json!({
            "query": query,
            "limit": limit,
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        let resp: TdaiResponse<Value> = self.post("/v3/atomic/search", &body).await?;
        let data = resp.data.ok_or_else(|| format!("atomic/search failed: {}", resp.message))?;
        let items = data.get("items").and_then(|m| serde_json::from_value::<Vec<AtomicItem>>(m.clone()).ok()).unwrap_or_default();
        Ok(items)
    }

    /// List L2 scenario files.
    pub async fn scenario_ls(&self, path_prefix: &str) -> Result<Vec<ScenarioEntry>, String> {
        let mut body = serde_json::json!({
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        if !path_prefix.is_empty() {
            body.as_object_mut().unwrap().insert("path_prefix".into(), serde_json::json!(path_prefix));
        }
        let resp: TdaiResponse<Value> = self.post("/v3/scenario/ls", &body).await?;
        let data = resp.data.ok_or_else(|| format!("scenario/ls failed: {}", resp.message))?;
        let entries = data.get("entries").and_then(|m| serde_json::from_value::<Vec<ScenarioEntry>>(m.clone()).ok()).unwrap_or_default();
        Ok(entries)
    }

    /// Read a single L2 scenario file.
    pub async fn scenario_read(&self, path: &str) -> Result<ScenarioReadData, String> {
        let body = serde_json::json!({
            "path": path,
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        let resp: TdaiResponse<ScenarioReadData> = self.post("/v3/scenario/read", &body).await?;
        resp.data.ok_or_else(|| format!("scenario/read failed: {}", resp.message))
    }

    /// Read L3 core persona.
    pub async fn core_read(&self) -> Result<CoreReadData, String> {
        let body = serde_json::json!({
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        let resp: TdaiResponse<CoreReadData> = self.post("/v3/core/read", &body).await?;
        resp.data.ok_or_else(|| format!("core/read failed: {}", resp.message))
    }

    /// Search skills.
    pub async fn skill_search(&self, query: &str, top_k: i64) -> Result<Vec<SkillSummary>, String> {
        let body = serde_json::json!({
            "query": query,
            "top_k": top_k,
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        let resp: TdaiResponse<Value> = self.post("/v3/skill/search", &body).await?;
        let data = resp.data.ok_or_else(|| format!("skill/search failed: {}", resp.message))?;
        let items = data.get("items").and_then(|m| serde_json::from_value::<Vec<SkillSummary>>(m.clone()).ok()).unwrap_or_default();
        Ok(items)
    }

    /// List skills for the bound agent.
    pub async fn skill_list(&self, limit: i64) -> Result<Vec<SkillSummary>, String> {
        let body = serde_json::json!({
            "pagination": {"limit": limit},
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
        });
        let resp: TdaiResponse<Value> = self.post("/v3/skill/list", &body).await?;
        let data = resp.data.ok_or_else(|| format!("skill/list failed: {}", resp.message))?;
        let items = data.get("items").and_then(|m| serde_json::from_value::<Vec<SkillSummary>>(m.clone()).ok()).unwrap_or_default();
        Ok(items)
    }

    /// Get a skill by name.
    pub async fn skill_get_by_name(&self, name: &str) -> Result<SkillSummary, String> {
        let body = serde_json::json!({
            "team_id": self.config.team_id,
            "agent_id": self.config.agent_id,
            "user_id": self.config.user_id,
            "skill_name": name,
            "include_content": true,
        });
        let resp: TdaiResponse<SkillSummary> = self.post("/v3/skill/get-by-name", &body).await?;
        resp.data.ok_or_else(|| format!("skill/get-by-name failed: {}", resp.message))
    }

    /// Internal POST helper.
    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &Value) -> Result<TdaiResponse<T>, String> {
        let url = format!("{}{path}", self.config.base_url);
        let resp = self.client
            .post(&url)
            .headers(self.headers())
            .json(body)
            .send()
            .await
            .map_err(|e| format!("{path} request failed: {e}"))?;
        let bytes = resp.bytes().await.map_err(|e| format!("{path} read failed: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| {
            let text = String::from_utf8_lossy(&bytes);
            format!("{path} parse failed: {e} — body: {text:.200}")
        })
    }

    /// Raw POST that returns the full JSON value (for meta endpoints with
    /// dynamic schemas not covered by typed responses). Internal helper.
    pub async fn post_raw(&self, path: &str, body: &Value) -> Result<Value, String> {
        let url = format!("{}{path}", self.config.base_url);
        let resp = self.client
            .post(&url)
            .headers(self.headers())
            .json(body)
            .send()
            .await
            .map_err(|e| format!("{path} request failed: {e}"))?;
        let bytes = resp.bytes().await.map_err(|e| format!("{path} read failed: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| {
            let text = String::from_utf8_lossy(&bytes);
            format!("{path} parse failed: {e} — body: {text:.200}")
        })
    }
}

// ---------------------------------------------------------------------------
// Tests (live against the local MemoryCore instance)
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> TdaiClient {
        TdaiClient::new(TdaiConfig {
            base_url: "http://127.0.0.1:8420".into(),
            gateway_api_key: "local".into(),
            service_id: "default".into(),
            user_key: "sk-mem-byteai-admin-20260828-0001".into(),
            team_id: "team-sqq0z9dwqr".into(),
            agent_id: "agt-sqq0olozlk".into(),
            user_id: "usr-sqprsy08k6".into(),
            task_id: None,
            conversation_id: "sess-byteai-test".into(),
            enabled: true,
        })
    }

    #[tokio::test]
    #[ignore = "requires external MemoryCore gateway on :8420 (optional; native hub is default)"]
    async fn test_health() {
        let c = test_client();
        let h = c.health().await.unwrap();
        assert!(h.contains("ok"));
    }

    #[tokio::test]
    #[ignore = "requires external MemoryCore gateway on :8420"]
    async fn test_auth_verify() {
        let c = test_client();
        let a = c.auth_verify("sk-mem-byteai-admin-20260828-0001").await.unwrap();
        assert!(a.valid);
        assert!(a.user.is_some());
    }

    #[tokio::test]
    #[ignore = "requires external MemoryCore gateway on :8420"]
    async fn test_conversation_add_and_query() {
        let c = test_client();
        let sess = "sess-test-rust-client";
        // Add a message
        let add = c.conversation_add(sess, vec![
            serde_json::json!({"role": "user", "content": "Rust test message"}),
        ]).await.unwrap();
        assert_eq!(add.total_count, 1);
        // Query it back
        let msgs = c.conversation_query(sess, 10).await.unwrap();
        assert!(msgs.iter().any(|m| m.content.contains("Rust test message")));
        // Search
        let found = c.conversation_search("Rust test", 5).await.unwrap();
        assert!(found.iter().any(|m| m.content.contains("Rust test message")));
    }

    #[tokio::test]
    #[ignore = "requires external MemoryCore gateway on :8420"]
    async fn test_atomic_search() {
        let c = test_client();
        // Search for something — may be empty but shouldn't error
        let items = c.atomic_search("local-first", 5).await.unwrap();
        // L1 pipeline may not have run yet, so empty is OK
        let _ = items;
    }

    #[tokio::test]
    #[ignore = "requires external MemoryCore gateway on :8420"]
    async fn test_scenario_and_core() {
        let c = test_client();
        // L2 scenario ls (fresh db, will be empty)
        let entries = c.scenario_ls("").await.unwrap();
        let _ = entries;
        // L3 core read (fresh db, content will be null)
        let core = c.core_read().await.unwrap();
        assert!(core.content.is_none() || !core.content.as_deref().unwrap_or("").is_empty());
    }

    #[tokio::test]
    #[ignore = "requires external MemoryCore gateway on :8420"]
    async fn test_skill_search() {
        let c = test_client();
        // Fresh db — no skills yet, but shouldn't error
        let items = c.skill_search("code review", 5).await.unwrap();
        let _ = items;
    }
}