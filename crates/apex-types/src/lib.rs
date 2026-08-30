//! Shared wire types for the ByteAi (APEX) agent.
//!
//! These types model the OpenAI-compatible chat-completions wire format so that
//! any OpenAI-compatible endpoint works: OmniRoute, Ollama, LM Studio, vLLM,
//! OpenRouter, and hosted gateways. Provider-specific adaptations (Anthropic,
//! Gemini) arrive in a later phase behind the same `Message` model.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// Message roles in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments string as emitted by the model.
    pub arguments: String,
}

/// Tool definition (OpenAI `tools` entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON-schema object for parameters.
    #[serde(default = "empty_object")]
    pub parameters: Value,
}

fn empty_object() -> Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// A message in the conversation. Follows the OpenAI wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Assistant messages may carry tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool messages reference the call they answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name (for tool messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Reasoning content (o-series style) — preserved, not sent as `content`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: Some(text.into()), tool_calls: None, tool_call_id: None, name: None, reasoning: None }
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: Some(text.into()), tool_calls: None, tool_call_id: None, name: None, reasoning: None }
    }
    pub fn assistant(content: Option<String>, tool_calls: Option<Vec<ToolCall>>, reasoning: Option<String>) -> Self {
        Self { role: Role::Assistant, content, tool_calls, tool_call_id: None, name: None, reasoning }
    }
    pub fn tool(tool_call_id: impl Into<String>, name: impl Into<String>, output: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: Some(output.into()), tool_calls: None, tool_call_id: Some(tool_call_id.into()), name: Some(name.into()), reasoning: None }
    }

    /// Serialize for the wire, mapping `reasoning` to `reasoning_content`
    /// where the endpoint expects it (OpenAI o-series compat).
    pub fn to_wire(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("role".into(), serde_json::json!(self.role));
        if let Some(c) = &self.content {
            m.insert("content".into(), serde_json::json!(c));
        } else if self.role == Role::Assistant {
            // DeepSeek thinking mode (and other reasoning proxies) REJECT
            // `content: null` on assistant messages — it demands a string,
            // even an empty one, so reasoning_content can be echoed back.
            // A tool-round assistant message (tool_calls, no text) must
            // therefore send `content: ""`, not null, or the provider 400s
            // mid-turn ("The request is invalid").
            m.insert("content".into(), serde_json::json!(""));
        }
        if let Some(tc) = &self.tool_calls {
            let wire: Vec<Value> = tc.iter().map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "type": "function",
                    "function": { "name": t.name, "arguments": t.arguments }
                })
            }).collect();
            m.insert("tool_calls".into(), serde_json::json!(wire));
        }
        if let Some(id) = &self.tool_call_id {
            m.insert("tool_call_id".into(), serde_json::json!(id));
        }
        if let Some(n) = &self.name {
            m.insert("name".into(), serde_json::json!(n));
        }
        if let Some(r) = &self.reasoning {
            m.insert("reasoning_content".into(), serde_json::json!(r));
        }
        Value::Object(m)
    }
}

/// Token usage accounting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// Result of executing one tool call.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub call_id: String,
    pub name: String,
    pub output: String,
    pub ok: bool,
    /// Elapsed milliseconds.
    pub elapsed_ms: u64,
}

/// Result of one agent run.
#[derive(Debug, Clone, Default)]
pub struct AgentOutcome {
    pub final_text: String,
    pub usage: Usage,
    pub tool_calls_made: u32,
    pub iterations: u32,
    pub finished: bool,
    /// True when the model asked the user a question (clarification or a
    /// choice) and the turn PAUSED to wait for the user's answer (CAP —
    /// Coding Auto-Pilot — off). `final_text` holds the question. The
    /// caller shows it and feeds the user's answer back as the next user
    /// message; the conversation history already contains the question.
    pub needs_input: bool,
    pub blocked_reason: Option<String>,
    /// True when the turn was stopped by an interaction budget (iteration
    /// cap or wall-clock run budget) but still produced a final answer from
    /// partial progress (graceful wrap-up, Hermes-style — but better: we
    /// record WHY it stopped).
    pub exhausted: bool,
    pub exhausted_reason: Option<String>,
}

/// A provider endpoint configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
}

/// Serializable session (for save/resume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFile {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub model: String,
    pub provider: String,
    pub messages: Vec<Message>,
    pub usage: Usage,
}
