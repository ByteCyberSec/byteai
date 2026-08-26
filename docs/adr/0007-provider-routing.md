# ADR-0007: Provider Routing

Status: accepted
Date: 2026-08-25

## Context
Models vary in speed, cost, and capability. A coding agent makes calls for many
roles (fast iteration, deep reasoning, code generation, review, vision, search,
planning, debugging). Routing by provider name alone is insufficient; routing by
capability role, then cheapest sufficient model, reduces cost and latency without
hurting success rate.

## Decision
Capability-based routing. Models declare roles (FAST, SMART, DEEP, CODE, REVIEW,
VISION, SEARCH, PLANNER, DEBUGGER, ARCHITECT, TINY). The router selects:
task → required capability → cheapest sufficient model (by cost and latency),
with explicit user override. The router learns from local statistics:
model_success_rate, edit_success_rate, debug_success_rate, median_latency,
median_tokens, retry_rate, cost_per_success. The primary metric is
**cost and time per successful task**, not cost per token.

Provider layer supports: OpenAI, Anthropic, Gemini, OpenRouter, Ollama, LM Studio,
vLLM, custom OpenAI-compatible endpoints (local-first: OmniRoute at :20128).

## Alternatives
- Provider-name routing: rejected — conflates capability with vendor.
- Always-strongest-model: rejected — expensive, slow, unnecessary for many tasks.
- Static role assignment: rejected — does not learn from outcomes.

## Tradeoffs
- Routing adds a small decision layer; saves cost/latency on most calls.
- Requires outcome tracking per model per task class (stats table).

## Consequences
- Router in `apex-router/` crate; provider abstraction in `apex-provider/`.
- Stats persisted in SQLite (`apex-session/` schema extension).
- User can pin a model per role (`/model`).