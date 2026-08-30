//! `moa` — Mixture of Agents: query N models in parallel, then route all
//! responses to a synthesizer model for a distilled answer.
//!
//! Uses the same OmniRoute provider (`byteai-provider::Client`) the rest of
//! ByteAI uses.  No tools passed to sub-queries (just text generation). The
//! synthesizer sees every sub-model's raw response and produces a refined answer.

use std::sync::Arc;
use std::time::Instant;

use byteai_provider::Client;
use byteai_types::{Message, ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct MoaTool {
    pub client: Arc<Option<Client>>,
    pub default_model: Arc<String>,
}

impl MoaTool {
    pub fn new(client: Arc<Option<Client>>, default_model: Arc<String>) -> Self {
        Self { client, default_model }
    }
}

impl Tool for MoaTool {
    fn name(&self) -> &'static str {
        "moa"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "moa".into(),
            description: "Mixture of Agents: queries N models in parallel for a prompt, then routes all responses to a synthesizer model for a refined answer. Great for reasoning-heavy questions where a diversity of perspectives helps. Input: {prompt, models?, synthesizer?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "the question to answer"},
                    "models": {"type": "array", "items": {"type": "string"}, "description": "model names to query (default: 3 auto-selected from available 'chat' models)"},
                    "synthesizer": {"type": "string", "description": "model name for the final synthesis step (default: the largest/best available model)"}
                },
                "required": ["prompt"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let client = self.client.clone();
        let default_model = self.default_model.clone();
        Box::pin(async move {
            let started = Instant::now();
            let prompt = args.get("prompt").and_then(Value::as_str).unwrap_or("").to_string();
            if prompt.trim().is_empty() {
                return ok_outcome("", "moa", "usage: moa {\"prompt\": \"...\"} [models] [synthesizer]", 0);
            }

            let Some(ref client) = *client else {
                return ok_outcome("", "moa", "moa requires a configured provider (OmniRoute or OpenAI-compatible endpoint)", 0);
            };

            // Resolve available models, pick N sub-models and a synthesizer.
            let models = match client.list_models().await {
                Ok(m) if m.is_empty() => vec![default_model.to_string()],
                Ok(m) => m,
                Err(_) => vec![default_model.to_string()],
            };
            let sub_count = 3usize.min(models.len());
            let sub_models: Vec<String> = models[..sub_count].to_vec();
            let synth_model = models.first()
                .cloned()
                .unwrap_or_else(|| default_model.to_string());

            // Run sub-queries in parallel (limited by semaphore).
            let sem = Arc::new(Semaphore::new(8));
            let mut handles = Vec::new();
            for m in &sub_models {
                let c = client.clone();
                let pm = prompt.clone();
                let model = m.clone();
                let s = sem.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = s.acquire().await.ok()?;
                    let msgs = vec![Message::system("You are a helpful assistant. Answer concisely."), Message::user(&pm)];
                    match c.chat(&model, &msgs, &[], Some(2048)).await {
                        Ok((content, _tcalls, _usage)) => Some((model, content)),
                        Err(e) => Some((model, format!("[error: {e:#}]"))),
                    }
                }));
            }

            let mut sub_responses: Vec<(String, String)> = Vec::new();
            for h in handles {
                if let Some(r) = h.await.unwrap_or(None) {
                    sub_responses.push(r);
                }
            }

            if sub_responses.is_empty() {
                return ok_outcome("", "moa", "all sub-query models failed", started.elapsed().as_millis() as u64);
            }

            // Build the synthesis prompt.
            let mut synth_prompt = String::from("You are a synthesis agent. Below are responses from multiple AI models to the same question. Synthesize them into a single coherent, well-structured answer, reconciling differences and filling gaps.\n\n");
            synth_prompt.push_str(&format!("## Original question\n{prompt}\n\n## Model responses\n"));
            for (m, r) in &sub_responses {
                let preview = if r.len() > 2000 {
                    format!("{}…\n  [response truncated at 2000 chars]", &r[..2000])
                } else {
                    r.clone()
                };
                synth_prompt.push_str(&format!("\n### Model: {m}\n{preview}\n"));
            }

            let msgs = vec![Message::system("You are a synthesis agent."), Message::user(&synth_prompt)];
            match client.chat(&synth_model, &msgs, &[], Some(4096)).await {
                Ok((content, _tcalls, _usage)) => {
                    let mut out = String::new();
                    out.push_str(&format!("MoA synthesis ({sub_count} models → {synth_model}):\n\n{content}\n\n"));
                    out.push_str("── sources ──\n");
                    for (m, _) in &sub_responses {
                        out.push_str(&format!("  ⋅ {m}\n"));
                    }
                    ok_outcome("", "moa", out, started.elapsed().as_millis() as u64)
                }
                Err(e) => ok_outcome("", "moa", format!("synthesis step failed: {e:#}"), 0),
            }
        })
    }
}