# ADR-0004: Context Management

Status: accepted
Date: 2026-08-25

## Context
Context is a scarce compute resource. Models have fixed context windows (typically 200K tokens). Compaction decisions directly affect task success, token cost, and response latency.

## Decision
Context management follows jcode's threshold ladder: 80% soft threshold triggers asynchronous compaction, 95% critical threshold triggers synchronous hard-compaction. Keep the last 10 turns verbatim. Use a flat image token cost (1,600 tokens per image — learned from jcode's discovery that base64-length accounting causes compaction thrash). Emergency compaction caps tool results at 4,000 chars and images at 1,024 chars. Maintain a 12 MB payload budget for 413 recovery.

For heavy compaction, optionally use an auxiliary (cheap) model to summarize middle turns, protecting head (system prompt + tool definitions) and tail (recent turns).

## Alternatives
- No compaction: rejected — hits provider context limits.
- Only hard-compaction: rejected — loses too much context.
- Only aux-model summarization: rejected — expensive and slow for every compaction.

## Tradeoffs
- Threshold ladder is cheap (no model calls) for most cases.
- Aux-model summarization adds latency but preserves more information.
- Flat image token cost avoids the accounting bug jcode discovered.

## Consequences
- Constants live in `apex-context/` crate.
- Per-model context budgets from provider catalog.
- Checkpoint/rewind (spec §25) uses the same budget tracking.