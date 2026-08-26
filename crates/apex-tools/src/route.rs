//! `route` — task-type model routing with fallback chain.
//! Ported from AiMyWay's Layer-2 ModelRouter: pick the best model for the
//! task type (coding / reasoning / fast / vision / chat / architecture),
//! then expose the fallback chain.

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ToolContext};

pub struct RouteTool {
    pub ctx: ToolContext,
}

impl RouteTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }

    fn routing_rules(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("coding", "auto/best-coding"),
            ("reasoning", "auto/best-reasoning"),
            ("fast", "auto/best-fast"),
            ("vision", "auto/best-vision"),
            ("chat", "auto/best-chat"),
            ("architecture", "auto/pro-coding"),
            ("research", "auto/best-reasoning"),
        ]
    }
}

impl Tool for RouteTool {
    fn name(&self) -> &'static str {
        "route"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "route".into(),
            description: "Route a task to the best model for its type (coding/reasoning/fast/vision/chat/architecture/research) with provider fallback. Input: {type, task}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "type": {"type": "string", "enum": ["coding","reasoning","fast","vision","chat","architecture","research"], "description": "Task type"},
                    "task": {"type": "string", "description": "Task description for context"}
                },
                "required": ["type"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let t = args.get("type").and_then(Value::as_str).unwrap_or("chat").to_string();
            let _task = args.get("task").and_then(Value::as_str).unwrap_or("").to_string();

            // 1. Match task type to a preferred model alias.
            let alias = RouteTool::new(ctx.clone())
                .routing_rules()
                .iter()
                .find(|(k, _)| k == &t)
                .map(|(_, m)| *m)
                .unwrap_or("auto/best-chat");

            // 2. If we have a provider, resolve the alias to a concrete model.
            let (mut model, mut provider) = (ctx.default_model.clone(), "default".to_string());
            let mut fallbacks: Vec<String> = Vec::new();

            if let Some(client) = &ctx.client {
                provider = "provider".to_string();
                if let Ok(ids) = client.list_models().await {
                    let tok = alias.trim_start_matches("auto/");
                    if let Some(m) = ids.iter().find(|m| m.as_str() == alias) {
                        model = m.clone();
                    } else if let Some(m) = ids.iter().find(|m| m.contains(tok)) {
                        model = m.clone();
                    } else if !ctx.default_model.is_empty() {
                        model = ctx.default_model.clone();
                    }
                    fallbacks = ids.iter().take(3).cloned().collect();
                }
            }

            let out = json!({
                "task_type": t,
                "alias": alias,
                "model": model,
                "provider": provider,
                "fallbacks": fallbacks,
                "reasoning": format!("task type '{}' -> {} (resolved to '{}')", t, alias, model),
            });
            crate::ok_outcome("", "route", out.to_string(), started.elapsed().as_millis() as u64)
        })
    }
}
