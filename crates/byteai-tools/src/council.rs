//! `council` — multi-model deliberation with supermajority voting.
//! Ported from AiMyWay's C-Suite / Governance council: send the same
//! question to 3-4 models, collect votes, require 75% supermajority.

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ToolContext};

pub struct CouncilTool {
    pub ctx: ToolContext,
}

impl CouncilTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for CouncilTool {
    fn name(&self) -> &'static str {
        "council"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "council".into(),
            description: "Multi-model deliberation: query 3-4 models on the same question, collect votes, return supermajority verdict. Input: {question, models?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question to deliberate"},
                    "models": {"type": "array", "items": {"type": "string"}, "description": "Optional: specific models to query (default: 3 diverse models from provider)"}
                },
                "required": ["question"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let question = args.get("question").and_then(Value::as_str).unwrap_or("").to_string();
            if question.is_empty() {
                return crate::err_outcome("", "council", &anyhow::anyhow!("missing 'question'"), 0);
            }

            let client = match &ctx.client {
                Some(c) => c.clone(),
                None => return crate::ok_outcome("", "council", "No provider client available — council requires a configured provider.", started.elapsed().as_millis() as u64),
            };

            // Pick council models: user-specified, or 3 diverse defaults.
            let council_models: Vec<String> = if let Some(ms) = args.get("models").and_then(Value::as_array) {
                ms.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            } else {
                let mut fallback = vec![ctx.default_model.clone()];
                if let Ok(ids) = client.list_models().await {
                    for id in &ids {
                        if fallback.len() >= 4 { break; }
                        if !fallback.contains(id) {
                            let name = id.to_lowercase();
                            // Keep the council diverse: 2nd seat = a reasoning
                            // model, 3rd = a fast one, 4th = anything else.
                            let want = match fallback.len() {
                                1 => name.contains("best-reasoning") || name.contains("reasoning"),
                                2 => name.contains("best-fast") || name.contains("mini") || name.contains("flash"),
                                3 => true,
                                _ => false,
                            };
                            if want {
                                fallback.push(id.clone());
                            }
                        }
                    }
                    for id in &ids {
                        if fallback.len() >= 4 { break; }
                        if !fallback.contains(id) { fallback.push(id.clone()); }
                    }
                }
                fallback.truncate(4);
                fallback
            };

            if council_models.is_empty() {
                return crate::ok_outcome("", "council", "No models available for council — configure a provider first.", started.elapsed().as_millis() as u64);
            }

            let mut votes = Vec::new();
            for model in &council_models {
                let prompt = format!(
                    "You are a council member. Answer ONLY with a valid JSON object with keys: vote (true/false), reasoning (string), confidence (0.0-1.0).\nQuestion: {question}\nAnswer:"
                );
                let msg = byteai_types::Message::user(&prompt);
                match client.chat(model, &[msg], &[], None).await {
                    Ok((text, _, _)) => {
                        let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
                            let lower = text.to_lowercase();
                            let yes = lower.contains("true") || lower.starts_with("yes") || lower.starts_with("approve");
                            let no = lower.contains("false") || lower.starts_with("no") || lower.starts_with("reject");
                            json!({
                                "vote": yes && !no,
                                "reasoning": text.trim().chars().take(200).collect::<String>(),
                                "confidence": if yes || no { 0.6 } else { 0.4 }
                            })
                        });
                        votes.push(json!({
                            "model": model,
                            "vote": parsed.get("vote").and_then(|v| v.as_bool()).unwrap_or(false),
                            "reasoning": parsed.get("reasoning").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            "confidence": parsed.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.5),
                        }));
                    }
                    Err(e) => {
                        votes.push(json!({
                            "model": model,
                            "vote": false,
                            "reasoning": format!("error: {e:#}"),
                            "confidence": 0.0
                        }));
                    }
                }
            }

            let total = votes.len() as f64;
            let approvals = votes.iter().filter(|v| v.get("vote").and_then(Value::as_bool).unwrap_or(false)).count() as f64;
            let approved = total > 0.0 && (approvals / total) >= 0.75;

            let out = json!({
                "question": question,
                "models_consulted": council_models.len(),
                "approved": approved,
                "approval_rate": format!("{:.0}%", (approvals / total * 100.0)),
                "votes": votes,
                "dissent_count": (votes.len() - approvals as usize),
            });
            crate::ok_outcome("", "council", out.to_string(), started.elapsed().as_millis() as u64)
        })
    }
}