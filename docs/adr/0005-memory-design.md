# ADR-0005: Memory Design

Status: accepted
Date: 2026-08-25

## Context
Memory is essential for cross-session continuity and reducing repeated work. The reference projects show three approaches: SQLite-only (hermes), internal graph+embeddings (jcode), and Markdown+SQLite (ai-memory). ai-memory's "compile, not retrieve" is the strongest pattern.

## Decision
Hybrid memory system with four layers:
- Layer A (working state): tiny, in-memory, contains current objective/plan/progress/blockers/next action.
- Layer B (project knowledge): Markdown files in `.byteai/memory/wiki/`, durable facts, architecture, conventions, testing commands, traps.
- Layer C (episodic): session observations compiled into Markdown summaries + SQLite/FTS5 index.
- Layer D (procedural): skills (SKILL.md files).

Storage: Markdown is source of truth (editable by hand, grep-able, version-controlled). SQLite is the derived index: FTS5, entities, entity graph, metadata, timestamps, optional embeddings. Embeddings are OPTIONAL (lazy-loaded, provider-agnostic).

Retrieval pipeline: exact/entity match → FTS → graph neighbors → optional vector similarity → reranking → relevance verification.

Conflict handling: each durable memory entry supports source/created_at/updated_at/confidence/scope/expiration/superseded_by. Conflicts are resolved or marked; operations: forget, invalidate, replace, merge, expire.

## Alternatives
- SQLite-only: rejected — lacks human-readable source of truth.
- Internal graph only (jcode): rejected — not human-editable.
- Everything in embeddings: rejected — fragile, expensive, not grep-able.

## Tradeoffs
- More storage code; vastly better cross-session continuity.
- Embeddings are optional — the system works without them.
- Markdown wiki is user-editable and portable.

## Consequences
- Memory module lives in `byteai-memory/` crate.
- SQLite schema in `byteai-session/` for session store; `byteai-memory/` for memory index.
- Optional companion process: ai-memory can be used for heavy cross-harness memory.