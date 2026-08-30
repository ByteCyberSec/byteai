# hermes-agent — Research Notes

> Source: https://github.com/NousResearch/hermes-agent (clone at `research/repos/hermes-agent`, 2026-08-25)
> I am currently running on Hermes Agent, so this analysis is informed by both source inspection and runtime experience.
> Document purpose: Phase 0 research for ByteAi (ByteAi). Verified by reading source layout + key files.

## 1. Overview

- **What**: A personal AI agent that runs the same core across CLI, messaging gateway (20+ platforms: Telegram, Discord, Slack, WhatsApp, etc.), TUI, and Electron desktop app. Self-improving through skills, persistent memory, delegated subagents, scheduled jobs, terminal + browser driving, and MCP extensibility.
- **Language**: Python (main agent) + Python/eel (desktop), Python/Textual (TUI), TypeScript/React (desktop app). 9,450 text files, ~2.8M LOC (but tests alone are 866K, apps 480K, optional-skills 160K — the true core is smaller: agent/ 152K, hermes_cli/ 246K, tools/ 147K, gateway/ 115K).
- **License**: MIT, Copyright (c) 2025 Nous Research.
- **Positioning**: The most feature-complete open-source agent framework. Every feature ByteAi needs exists somewhere in Hermes (skills, memory, sessions, delegation, MCP, plugins, hooks, gateway, cron, profiles, security, streaming, provider pools). The cost: Python, 2.8M LOC, 10K+ files.

## 2. Architecture

```
hermes-agent/
├── agent/                 — Core agent loop: conversation_loop.py (8,598), context_compressor.py (8,415), context_engine.py, auxiliary_client.py (10,880 — aux model calls), chat_completion_helpers.py (5,405), turn_context.py, turn_retry_state.py, error_classifier.py, message_sanitization.py, message_metadata.py, redact.py, model_metadata.py, display.py, runtime_cwd.py, process_bootstrap.py, codex_responses_adapter.py, conversation_compression.py
├── hermes_state.py        — (14,650) Session/state engine: state.db schema, memory tool, user profile, memory injection per turn
├── cli.py                 — (21,665) CLI entry point
├── run_agent.py           — (9,215) Agent bootstrap
├── hermes_cli/            — CLI support: main.py (14,462), web_server.py (19,855), auth.py (9,520), models.py (7,039), config.py (6,072), tools_config.py (6,017), plugins.py (6,782), gateway.py (8,428), kanban_db.py (12,149), update_cmd.py
├── tools/                 — Tool implementations: mcp_tool.py (8,592), approval.py (5,727), browser_tool.py (5,602), todo_tool.py, etc.
├── gateway/               — Messaging gateway: run.py (31,593), platforms/* (telegram, discord, slack, feishu, matrix, whatsapp, signal, sms, email, etc.), slash_commands.py (6,136), api_server.py
├── cron/                  — Scheduled jobs: scheduler.py (7,809)
├── acp_adapter/           — IDE protocol adapter (ACP) for VS Code/Zed/JetBrains
├── apps/                  — Desktop app (Electron + React, 480K LOC)
├── ui-tui/                — Ink TUI (474 files, 97K LOC)
├── tui_gateway/           — TUI gateway server (16,433)
├── skills/                — 485 files, 125K LOC of skill SKILL.md files
├── optional-skills/       — 551 files, 160K LOC (optional skill packs)
├── plugins/               — 339 files, 142K LOC: memory/openviking, platforms/*, etc.
├── providers/             — Provider definitions (3 files)
├── native/                — Native (Rust? 3 files, 295 LOC)
├── evals/                 — 30 files, 3,712 LOC
├── tests/                 — 3,373 files, 866K LOC
└── docs/                  — llms.txt, llms-full.txt
```

## 3. Mechanism Analysis

### Agent loop (`agent/conversation_loop.py` 8,598)
- what: One user turn through the agent: model call → tool dispatch → retries → fallbacks → compression → post-turn hooks → background memory/skill review nudges. Extracted from the 9,215-line `run_agent.py`.
- approach: `AIAgent.run_conversation` drives the loop; `TurnRetryState` manages retry state; `error_classifier` provides `FailoverReason` (classify_api_error); `message_sanitization` sanitizes messages (non-ascii, surrogates, stale tool-call markers, tool-arg repair); `context_engine` provides compaction status; `model_metadata` estimates tokens and detects provider errors (context length, output cap).
- key invariants: strict message role alternation; prompt caching is sacred (never mutate past context, swap toolsets, or rebuild system prompt mid-conversation — the one exception is context compression).
- ByteAi copy: the retry/fallback state machine (`TurnRetryState`); error classification (`FailoverReason`); prompt caching inviolability; the "narrow waist" philosophy (core = small, capability at edges).
- ByteAi improve: explicit phase state machine (UNDERSTANDING/INVESTIGATING/IMPLEMENTING/VERIFYING/...) with tool policies per phase (ByteAi §45); the loop should be cleaner in Rust.

### Context compression (`agent/context_compressor.py` 8,415)
- what: Automatic context window compression using an auxiliary model (cheap/fast) to summarize middle turns while protecting head and tail. Structured summary template with Resolved/Pending question tracking. Iterative summary updates. Token-budget tail protection. Tool output pruning before LLM summarization. Scaled summary budget.
- approach: Uses `AuxiliaryExplicitCancellation` for interruptible summarization; `redact_sensitive_text` before sending to aux model; `estimate_messages_tokens_rough` for budget; TODO_INJECTION_HEADER awareness.
- ByteAi copy: aux-model-based summarization with head/tail protection; structured summary (Resolved/Pending); iterative updates; token-budget tail protection.
- ByteAi improve: adopt jcode's threshold constants (80% soft, 95% hard, keep 10 turns, flat image token cost); add per-model budgets from catalog.

### Memory (`hermes_state.py` 14,650; `agent/` memory integration)
- what: `state.db` SQLite + FTS5 session store. Memory tool writes to user profile + personal notes. 2,200-char budget per store (injected every turn). Session search backed by FTS5. Skills store (SKILL.md files).
- approach: `hermes_state.py` owns the DB schema; `memory` tool provides CRUD; `session_search` provides FTS5 retrieval. User profile is separate from the memory tool.
- ByteAi copy: FTS5 session search; dual-store (user profile + personal notes); budget-gated injection; separate memory/skills/prompts directories.
- ByteAi improve: add ai-memory's compile-not-retrieve pattern; add entity graph; add optional embeddings; add conflict resolution/explicit metadata. ByteAi's memory should be a module inside the Rust core, not a Python module.

### Skills (`skills/` 485 files, `optional-skills/` 551 files, `skill_manage` tool)
- what: SKILL.md format (YAML frontmatter + markdown body); lifecycle: create → use → patch/delete; skill_manage tool; curriculum/promotion (skills can be curated from lessons). The user profile + memory + skills form a three-layer knowledge system.
- ByteAi copy: the SKILL.md format (ByteAi §17); the lifecycle (experience → candidate → reuse → validation → promotion); the skill_manage API.
- ByteAi improve: versioned skills with verification; dependency resolution; skill testing; skill blocking (mark conflict as BLOCKER). ByteAi's skill system should be integrated into the Rust core, not Python.

### Delegation (`delegate_task` tool)
- what: spawn subagents in isolated contexts; each gets a separate conversation, terminal session, and toolset; only the final summary returns. Batch mode (up to 10 concurrent). `output_schema` validation of subagent results. `leaf` vs `orchestrator` roles. `steer`/`stop`/`list` controls. Worktree mode (`-w`) for git isolation.
- ByteAi copy: the delegation API (spawn/steer/stop/list, batch, output_schema, leaf/orchestrator, concurrency caps); worktree isolation; typed result schemas.
- ByteAi improve: make subagent results queryable (ByteAi §11 — structured schema with findings/files/tests/risks/confidence); separated worktree with conflict detection (ByteAi §10); explicit subagent budgets.

### Hooks & events
- what: PreToolUse hook (security / policy enforcement); event system (internal event bus mentioned in AGENTS.md philosophy). The AGENTS.md explicitly says: "A hook is NOT speculative if a contributor has a real, stated use case — even if the consumer ships separately."
- ByteAi copy: the hook discipline (only add hooks for concrete consumers); PreToolUse pattern for security gating.
- ByteAi improve: full event bus (ByteAi §31: session.started, task.started, tool.started, file.changed, diagnostic.created, test.failed, agent.spawned, memory.created, etc.).

### MCP (`tools/mcp_tool.py` 8,592)
- what: MCP server support (catalog, `hermes mcp` commands). MCP is used for external extensibility, not for internal tools.
- ByteAi copy: MCP for external extensibility only; core tools are native (ByteAi §32).
- ByteAi improve: MCP should be an optional module (not loaded on the hot path).

### Providers (`agent/chat_completion_helpers.py`, `agent/auxiliary_client.py`, `hermes_cli/models.py`, `providers/`)
- what: 20+ providers (OpenAI, Anthropic, Google, DeepSeek, xAI, OpenRouter, Ollama, vLLM, etc.); credential pools that rotate across API keys; model routing (user-selected, not role-based learned); aux client for cheap model calls (compression, tool result summary).
- ByteAi copy: credential pools + rotation; aux client pattern (separate model for cheap work); model metadata.
- ByteAi improve: role-based routing (FAST/SMART/CODE/...) with learned per-task-class stats (ByteAi §3); cheapest-sufficient selection; catalog-driven model metadata.

### Security (`agent/redact.py`, `agent/message_sanitization.py`, `tools/approval.py`, `hermes_cli/auth.py`)
- what: secret redaction (redact_sensitive_text); message sanitization (non-ascii, surrogates, stale tool-call markers); approval modes (interactive, auto); command approval (`tools/approval.py` 5,727); path restrictions; OAuth/auth.
- ByteAi copy: secret redaction; approval mode + command approval; path restrictions; sanitization pipeline.
- ByteAi improve: prompt-injection detection on repo content and tool output; permission scopes for subagents; sandbox options.

### Sessions & storage (`hermes_state.py`, `cron/scheduler.py`)
- what: `state.db` SQLite + FTS5; session search; session storage with JSONL transcripts; session resume; cron scheduling with script + no_agent mode.
- ByteAi copy: FTS5 session store; session search; cron scheduling pattern (no_agent mode, monitor scripts, script-based jobs).
- ByteAi improve: keep session engine in Rust core; make it embeddable (no external process).

### Observability (`hermes_cli/web_server.py`, `doctor`)
- what: `hermes doctor` health check; web dashboard; TUI status; logs in `~/.hermes/logs/`.
- ByteAi copy: doctor command; observability dashboard.
- ByteAi improve: event-bus-driven telemetry (ByteAi §31); agent hub + agent event bus.

## 4. Performance Profile

- **Startup**: Measurable (Python). 10K+ files. The entry point loads ~hundreds of modules. Not competitive with Rust targets.
- **Memory**: Python cost baseline. The `agent/` + `hermes_cli/` + `tools/` + `gateway/` total ~500K+ LOC Python loaded at startup.
- **Lazy loading**: skills are file-system based and loaded on demand; optional features are service-gated.
- **Fundamental**: Python is the wrong language for ByteAi's performance targets. Hermes's architecture DESIGN is the right reference; the IMPLEMENTATION language is wrong for ByteAi.

## 5. Verdict for ByteAi

**Copy conceptually (top 5)**
1. "Narrow waist" core + capability at edges philosophy (the Footprint Ladder: extend existing → CLI+skill → service-gated tool → plugin → MCP catalog → last resort core tool).
2. Skills lifecycle (create → use → patch → promote; SKILL.md format; skill_manage API; lesson → candidate → promotion).
3. Delegation API (spawn/steer/stop/list, batch, output_schema, leaf/orchestrator, worktree isolation, concurrency limits).
4. FTS5 session store + session search; memory budget-gated injection.
5. Security: secret redaction, approval modes, sanitization pipeline; prompt caching as an inviolable invariant.

**Weaknesses**
- Python → 2.8M LOC, 10K+ files, 866K LOC of tests. Startup time, memory, and complexity are not competitive with Rust.
- Everything is a Python import — no lazy binary loading, no AOT compilation, no static linking.
- The core files are enormous (conversation_loop.py 8,598, context_compressor.py 8,415, auxiliary_client.py 10,880, hermes_state.py 14,650, cli.py 21,665, gateway/run.py 31,593).
- No role-based routing (user selects model; no capability-based router).
- No DAP, no LSP-grade code intelligence, no tiered search, no multi-strategy edit engine.
- The agent loop is a single giant function (3,900 lines of `run_conversation`).

**Improve**: reimplement in Rust; keep the narrow-waist philosophy; adopt the skills lifecycle; adopt the delegation API; adopt the FTS session store; add role-based routing, LSP/DAP, tiered search, multi-strategy editing.

**Reject**: Python implementation; the 31K-line gateway/run.py; 10K+ file project structure; the bespoke message-alternation invariants (use standard JSON-RPC); the 866K LOC of tests (ByteAi should be more test-concise).

**Reuse score: 9/10 as design philosophy reference; 1/10 as fork base.** Hermes is conceptually the closest project to ByteAi's vision, but it's Python and 2.8M LOC. ByteAi must reimplement the architecture in Rust. The SKILL.md files, delegation API, and session store schema can be directly ported.