# ai-memory — Research Notes

> Source: https://github.com/akitaonrails/ai-memory (clone at `research/repos/ai-memory`, 2026-08-25)
> Document purpose: Phase 0 research for ByteAi (ByteAi). Verified by reading source layout + key files.

## 1. Overview

- **What**: A standalone Rust binary that gives AI coding agents long-term, cross-session memory over MCP and lifecycle hooks. Supports 20+ coding agents (Claude Code, Codex, OpenCode, Cursor, OMP, Pi, Devin, Grok, Kimi Code, Antigravity, etc.). Markdown-in-git is the source of truth; SQLite is the derived index. "Compile, not retrieve" (Karpathy-style).
- **Language**: Rust (edition 2024, tokio, axum, rmcp, rusqlite, git2, refinery). ~232K LOC across 572 files.
- **Size**: 572 text files, ~232K LOC. Core crates: ai-memory-store (6,034 lib + 8,633 reader + 2,688 writer + 8,005 ops), ai-memory-mcp (11,150 server + 10,362 admin + 1,692 auth), ai-memory-hooks (11,968 router + 2,375 payload + 1,453 capture_policy + 1,383 workstream), ai-memory-consolidate (3,412 auto_improve + 2,848 consolidator + 1,700 bootstrap), ai-memory-cli (8,668 install_hooks + 2,905 serve + 2,549 hook + 2,437 install_mcp + 2,304 uninstall + 2,277 hook_spool + 2,181 run + 1,092 show + 1,013 hook_capture + 3,032 render_shared), ai-memory-core (active_project.rs 1,292), ai-memory-workstream (4,993 transcript + 1,876 harness), ai-memory-wiki (wiki.rs 2,254), ai-memory-web (routes/api.rs 1,453 + mount).
- **License**: MIT, Copyright (c) 2026 Fabio Akita.
- **Positioning**: Zero-friction, cross-agent memory. Not a coding agent itself — a memory server that any coding agent talks to.

## 2. Architecture

```
ai-memory/
├── crates/ai-memory-store/     — SQLite schema (refinery migrations), FTS5, reader/writer/ops, auto_improve
├── crates/ai-memory-mcp/       — MCP server (rmcp + axum), admin endpoints, auth
├── crates/ai-memory-hooks/     — Lifecycle hook ingestion: router (11,968!), payload parsing, capture policy, workstream capture
├── crates/ai-memory-consolidate/ — LLM-driven consolidation: auto_improve, consolidator, bootstrap
├── crates/ai-memory-cli/       — CLI commands: install_hooks, serve, hook, install_mcp, run, uninstall, hook_spool, show, hook_capture, render_shared, config
├── crates/ai-memory-core/      — ActiveProject resolution
├── crates/ai-memory-workstream/ — Managed workstream: transcript, harness
├── crates/ai-memory-wiki/      — Wiki page management
├── crates/ai-memory-web/       — Web UI on /web
├── crates/ai-memory-llm/       — LLM provider abstraction (Anthropic, OpenAI, Gemini, Copilot, OIDC)
├── bin/                        — CLI entry points
├── companions/                 — Agent companion scripts (4 files, 2,634 LOC)
├── hooks/                      — 162 files of generated hook scripts for all supported agents
├── evals/                      — Evaluation suite
├── tests/                      — Integration tests
├── docker/                     — Dockerfile + compose
├── docs/                       — ARCHITECTURE.md, design-decisions, auto-improvement-loop, mcp-install, macos, windows, managed-workstreams, managed-harness-contributions
└── packaging/                  — Package scripts
```

## 3. Memory Model Deep Dive

### Source of truth: Markdown in git
- The wiki at `<data_dir>/wiki/` is plain markdown, sorted into topic directories.
- Every consolidation pass produces a git commit (via `git2` with vendored libgit2).
- `grep`-able, openable in Obsidian, backed up with `rsync`.
- "No vector database to babysit, no `write_note` ceremony."

### Derived index: SQLite
- `<data_dir>/db/memory.sqlite`, WAL mode.
- One writer actor owns the write connection; reads go through a read-only pool.
- Indexes: FTS5 (full-text search), sessions, observations, handoffs, users, audit log, entity/page links, embeddings, optional workstream ledger.
- Refinery migrations.

### Per-entry metadata
- **Observations**: bounded (16 KiB user prompts, 2 KB tool excerpts, 16 KiB durable backstop). Fields: `workspace_id`, `project_id`, `path`, `agent` (harness name), `session_id`, `event_kind` (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop, etc.), `captured_at`, `body` (sanitized), `body_size`, `tool_name`, `tool_input`, `role`.

### Retrieval pipeline
1. FTS5 (full-text search on observations + wiki pages)
2. Lexical entity-match (exact entity names)
3. Link-neighbor RRF (entity/page cross-references)
4. Optional vector RRF (when an embedding provider is configured: OpenAI, Voyage, Gemini, or OpenAI-compatible via Ollama/LM Studio/vLLM)
5. Bounded raw-observation fallback (if structured retrieval yields insufficient context)

The AGENTS.md documents: "Retrieval is FTS5 + lexical entity-match + link-neighbor RRF, with optional vector RRF when an embedding provider is configured, plus bounded raw-observation fallback."

### Embeddings
- **Truly optional** — the system works in zero-LLM mode without embeddings.
- Embedding providers: OpenAI, Voyage, Google Gemini, and keyless OpenAI-compatible endpoints such as Ollama, LM Studio, and vLLM.
- When absent: retrieval defaults to FTS5 + entity + RRF.

### Automatic consolidation
- LLM-driven (opt-in): session observations are compiled into durable wiki pages.
- "Compile, not retrieve" — the system authors coherent wiki pages from observations, not just retrieving raw tool dumps.
- **Consolidation** (`ai-memory-consolidate/crates/consolidator.rs` 2,848 lines): does the heavy lifting of turning observations into wiki pages.
- **Auto-improve** (`auto_improve.rs` 3,412 lines): self-improvement loop — reviews and refines memory quality.
- **Bootstrap** (`bootstrap.rs` 1,700 lines): initial memory population.

### LLM opt-in
- Zero-LLM mode: still captures, searches (FTS5), and writes rule-based summaries.
- Providers (Anthropic, OpenAI, OpenAI/Codex OAuth, GitHub Copilot, Gemini, OpenAI-compatible endpoints) enable consolidation, lint, and auto-improvement loop.
- LLM is used for memory consolidation and retrieval ranking, not for every action.

### Conflict handling
- Not explicitly found in the shallow scan (no `superseded_by` field visible in the observation schema). The auto-improvement loop may handle conflicts implicitly.
- **ByteAi gap**: ai-memory does not have explicit conflict resolution (forget/invalidate/replace/merge/expire). ByteAi must add this.

## 4. Session Handoff

- Automatic at session end: lifecycle hooks capture the final tool turn and the server compiles a handoff.
- `ai-memory run <agent>` provides managed workstreams: transparent cross-harness resume. The next agent (even a different one) receives a bounded handoff packet.
- Handoff format: not deeply inspected here, but documented as "coherent summary from relevant observations" with "portable visible-event ledger" for managed workstreams.

## 5. Hooks & Agent Integration

- **162 hook files** covering 20+ agents. Per-agent lifecycle hooks that POST sanitized, bounded observations to the server.
- Supported agents: Claude Code, Codex, Command Code, Devin CLI, OpenCode, Cursor, Gemini CLI, Oh My Pi, Pi, Crush, Kiro CLI, Grok Build CLI, Antigravity CLI, Kimi Code, Zero, Swival CLI, Pool, VS Code Copilot, Zed, Hermes (community).
- Hook events: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop, subagent events, failure events.
- MCP: `memory_handoff_accept`, `memory_install_self_routing`, etc.

## 6. Performance & Resource Profile

- **Rust binary**: single binary, no runtime dependency. ~10-20 MB binary.
- **Startup**: fast (Rust binary, SQLite WAL).
- **Memory**: moderate (SQLite is memory-mapped; embeddings are optional and lazily loaded).
- **Rust buys**: determinism, fast startup, small binary, cross-platform, no Python/Node dependency (crucial for ByteAi's "no Python/Node required" mandate).
- **Lazy vs eager**: embeddings are lazy (optional provider); LLM consolidation is on-timer or on-event; hooks are fire-and-forget bounded.

## 7. Tests & Evals

- `evals/` (8 files, 888 LOC) — evaluation suite.
- `tests/` (3 files, 751 LOC) — integration tests.
- `docs/ARCHITECTURE.md` — full operational map.
- `docs/auto-improvement-loop.md` — before changing auto-improvement.
- `docs/design-decisions.md` — historical rationale.

## 8. Weaknesses

1. **No explicit conflict resolution** (replace/merge/expire/supersede) — the auto-improvement loop may handle this implicitly, but there's no documented conflict operation.
2. **LLM still needed for quality consolidation** — zero-LLM mode works but produces rule-based summaries; the best memory quality comes from LLM consolidation, adding a dependency.
3. **Complexity**: 232K LOC for a memory server is substantial. The hook router alone is 11,968 lines. The MCP server is 11,150 lines.
4. **Per-project isolation is by `(workspace_id, project_id, path)`** — requires the agent to know its project context; this is well-designed but adds setup friction.
5. **No storage-level embeddings** — the embedding index is in SQLite, not a dedicated vector DB, which limits recall at very large scale.

## 9. Verdict for ByteAi

**Copy conceptually (top 5)**
1. Markdown as source of truth + SQLite/FTS5 as derived index ("compile, not retrieve"). This is the single best memory architecture among all reference projects.
2. Truly optional embeddings: zero-LLM mode works; retrieval works without vectors.
3. Cross-agent lifecycle hooks (20+ agents supported) — ByteAi can use the same hook pattern for its own agent interface.
4. Bounded observation capture (16 KiB prompts, 2 KB tool excerpts, 16 KiB backstop) — prevents memory bloat.
5. Per-project isolation by (workspace, project, path) — matches ByteAi's multi-project requirement.

**Improve**
1. Add explicit conflict resolution: `superseded_by`, `expires_at`, `forget/invalidate/replace/merge` operations.
2. Add a dedicated entity graph (ai-memory has entity/page links but not a full graph for querying).
3. Make the hook router simpler — 11,968 lines is too much for hook dispatch.
4. Keep the MCP server thin (ai-memory's MCP admin is 10,362 lines — ByteAi should keep MCP thin per §32).

**Reject**
- Forking the project: ai-memory is a standalone memory server, not a coding agent harness. ByteAi's memory subsystem should be a module inside the core, not a separate server — although ai-memory's MCP-surface design means it CAN be used as a companion process.
- The 162-file hook system: ByteAi needs a single, clean hook interface per agent type, not 162 files.
- Keeping the giant files (11K+ router, 10K+ admin, 8K+ reader, 8K+ ops) — ByteAi should split into focused modules.

**Reuse score: 8/10 as design reference; 6/10 as companion process.** The memory model (Markdown+SQLite+FTS+optional+embeddings+compile-not-retrieve) is the best design among all references and should be ByteAi's memory architecture. The actual binary can be used as a companion/memory-server (`ai-memory serve` → ByteAi talks to it over MCP), but ByteAi should also own a lightweight embedded store for sessions and working state directly (no external process dependency for basic operation).