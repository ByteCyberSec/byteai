# jcode — Research Notes

> Source: https://github.com/1jehuang/jcode (clone at `research/repos/jcode`, commit of 2026-08-25)
> Document purpose: Phase 0 research for APEX (ByteAi). Verified by reading source, not README alone.

## 1. Overview

- **What**: A RAM-efficient, IDE-grade autonomous coding-agent harness in Rust, with a daemon architecture, TUI, SDK, telemetry, multi-provider support, memory, and swarm coordination.
- **Language**: Rust (edition 2024, tokio), ~85 workspace crates. One Python helper (`score_shard.py`), an iOS app directory, and a telemetry-worker.
- **Size**: 1,785 text files scanned, ~785K LOC text (large share is tests: `live_tests.rs` 3,087, `onboarding_eval.rs` 3,303, provider E2E suites). The `assets/` dir is 169 MB (bundled binaries/icons) — not source.
- **License**: MIT, Copyright (c) 2025 Jeremy Huang. No NOTICE file found; third-party components live under normal cargo deps.
- **Positioning**: "The most RAM efficient harness / The most intelligent harness." Daemon + clients model (TUI, CLI, SDK, headless), self-hosting development loop (`jcode self-dev`), ambient mode, plan mode, swarm.

## 2. Architecture

```
jcode/
├── crates/jcode-base            — core library: memory, embedding facade, provider/mod.rs, auth/lifecycle, config, storage, safety, sidecar
├── crates/jcode-app-core        — server daemon: server.rs (2,389), client_lifecycle (3,300), swarm.rs (3,170), tool/{communicate 3,364, discover 2,981, todo 2,527, memory}, comm_* (channels/control/graph/plan/session/sync), live_turn, headless, reload/recovery
├── crates/jcode-tui             — TUI client: app/{input 4,084, commands 3,664, auth 3,522, state_ui 2,213, inline_interactive 4,516}, ui_messages 4,478, session_picker, split_view, remote (server_events, key_handling)
├── crates/jcode-tui-*           — render, markdown, mermaid, messages, style, permissions, usage-overlay, session-picker, tool-display, anim, visual-debug, workspace
├── crates/jcode-agent-runtime   — agent runtime
├── crates/jcode-swarm-core      — swarm types/validation (tldr protocol, MAX_SWARM_MEMBERS=1000)
├── crates/jcode-compaction-core — compaction policy constants + engine
├── crates/jcode-memory-types    — MemoryEntry/MemoryGraph/ranking/reinforcement types
├── crates/jcode-embedding       — ONNX embedder + cross-encoder reranker (heavy dep isolated for build caching)
├── crates/jcode-provider-*      — provider metadata + runtimes: openai, anthropic, gemini, openrouter, copilot, cursor, claude-cli, grok-build, bedrock, antigravity + provider-doctor (E2E), provider-core, provider-env
├── crates/jcode-storage         — session storage
├── crates/jcode-protocol        — protocol types (protocol_memory.rs)
├── crates/jcode-tool-core/types — tool registry
├── crates/jcode-harness-api(-server) — SDK harness API (translate.rs 2,284)
├── crates/jcode-sdk             — public SDK
├── crates/jcode-fuzzy           — fuzzy search
├── crates/jcode-plan            — plan mode
├── crates/jcode-command-risk    — command approval risk scoring
├── crates/jcode-telemetry-core  — telemetry
├── src/                         — CLI entry (cli/commands.rs 3,482; bin/memory_recall_bench.rs 2,667)
├── telemetry-worker/            — separate telemetry process
├── sdk/                         — SDK layer
├── scripts/                     — benchmark_discovery.py, benchmark_attribution.py, install scripts, swallowed_error_budget.json
└── docs/                        — MEMORY_ARCHITECTURE.md, HOOKS.md, AMBIENT_MODE.md, HERDR.md, DISCOVERY_BENCHMARK.md, ...
```

**Agent loop (as implemented)**: a long-lived daemon (`jcode-app-core/src/server.rs`) owns sessions; each client (TUI/CLI/SDK) connects over a socket; `client_lifecycle` manages per-client state; `live_turn` drives one turn (model call → tool dispatch → results); `comm_*` modules manage channel-based communication, plans, and sync; swarm coordination runs in `swarm.rs` with persistence (`swarm_persistence`) and mutation state. Headless sessions (`headless.rs`) support `jcode run '<prompt>'`. Self-dev/reload machinery (`reload*`) lets the daemon swap binaries safely — `AGENTS.md` documents that a fresh build is inert until the shared-server symlink is repointed.

## 3. Mechanism Analysis

### Agent loop (`jcode-app-core/src/server.rs`, `client_lifecycle.rs`, `live_turn.rs`)
- what: daemon-per-user serving many clients; turn = model call + tool dispatch, with background task dispatch, swarm broadcast, todo progress, tool activity.
- why: one warm process amortizes model/tool state across sessions; clients are thin.
- approach: socket server, per-client state machines, channel-based comm.
- deps: tokio, serde, own protocol crate.
- perf: startup cost paid once per daemon; per-client sessions cheap — this is the basis of the 27.8 MB/session claims.
- weaknesses: daemon lifecycle complexity (reload/recovery code is large); harder to reason about than a per-session process; socket protocol is a custom surface.
- APEX copy: daemon-with-thin-clients for multi-session efficiency; headless `run` mode; self-dev reload discipline.
- APEX improve: define the client protocol as a typed schema (JSON-RPC-like) instead of bespoke messages.
- APEX reject: the sheer size of the client-lifecycle/comm matrix (many bespoke modules) — APEX should keep the core loop small and push features to modules.

### Memory (`jcode-base/src/memory.rs` 2,065, `memory/*`, `memory_types`, `jcode-embedding`)
- what: persistent cross-session memory, project-scoped + global; graph memory (`MemoryGraph`), categories, scopes, trust levels, reinforcement; pending/injected memory pipeline; a Haiku sidecar for extraction and relevance verification; skill registry integrated as synthetic memory entries with retrieval bonus.
- why: relevance-gated injection — only scored, top-k (10 of 30 candidates) memories enter context; skills are memories.
- approach: `memory_graph` (versioned), `ranking::{top_k_by_ord, top_k_by_score}`, embedding facade with process-wide LRU (128) + lazy load/unload, cross-encoder rerank for (query, passage).
- deps: jcode-embedding (ONNX + tokenizer — heavy, isolated in own crate to preserve build cache), haiku sidecar (small model process).
- perf: embeddings are optional and lazily loaded; "local embedding off" saves ~140 MB at 10 sessions (117 vs 260.8 MB PSS). Embedding LRU prevents repeat inference.
- weaknesses: sidecar dependency adds a process; memory prompt construction is complex (`memory_prompt.rs`); consolidation/auto-improve not deeply inspected here.
- APEX copy: relevance-gated top-k injection; skill-as-memory (synthetic providers); optional lazy embeddings with LRU; cross-encoder reranking; trust/reinforcement metadata.
- APEX improve: make the sidecar pluggable (any cheap model); add explicit conflict metadata (supersedes/expiration) like ai-memory.
- APEX reject: coupling memory to one model family; unversioned prompt logic.

### Swarm (`jcode-app-core/src/server/swarm.rs`, `jcode-swarm-core`)
- what: multi-agent coordination with plans broadcast to members, status/events, channels/subscriptions, persistence, tldr enforcement on long messages.
- why: bounded message sizes keep swarm traffic cheap; plan broadcasting keeps agents aligned.
- approach: `MAX_SWARM_MEMBERS=1000` hard cap + RAM-budget soft cap; `SWARM_TLDR_REQUIRED_OVER_CHARS=240`, `MAX_SWARM_TLDR_CHARS=200`, `SWARM_COMPLETION_REPORT_MARKER`; validation of tldr vs body.
- deps: jcode-plan types.
- perf: token efficiency by construction (short inter-agent messages).
- weaknesses: swarm semantics are bespoke; deep integration with the server makes reuse hard.
- APEX copy: tldr discipline for inter-agent messages; typed completion reports; membership caps + budget gating.
- APEX improve: adopt a typed result schema per agent role (status/findings/files/tests/risks) instead of free-form marker strings.
- APEX reject: 1,000-member swarms as a target — APEX should start with small, genuinely-parallel teams.

### Compaction (`jcode-compaction-core/src/lib.rs` 1,036)
- what: token-budget accounting + compaction policy.
- why: deterministic thresholds; hard compact before provider 413.
- approach: `DEFAULT_TOKEN_BUDGET=200_000`, `COMPACTION_THRESHOLD=0.80`, `CRITICAL_THRESHOLD=0.95`, keep 10 recent turns verbatim, min 2 turns emergency; emergency tool-result cap 4,000 chars, image cap 1,024; flat `IMAGE_TOKEN_COST=1_600` (learned: base64-length accounting overestimates and causes compaction thrash); `PAYLOAD_IMAGE_CHAR_BUDGET=12MB` for Anthropic 413 recovery; `CHARS_PER_TOKEN=4`.
- deps: message-types.
- perf: prevents provider failures and token waste; image-cost fix is a genuinely useful lesson.
- weaknesses: single-provider assumptions in constants (200k = Claude); needs per-model budgets.
- APEX copy: the threshold ladder, verbatim recent-turn retention, flat image token cost, emergency truncation caps, 413 payload budget.
- APEX improve: per-model context budgets from a model catalog; adaptive compaction that measures real provider input tokens.
- APEX reject: hardcoded 200k.

### Tools (`jcode-app-core/src/tool/*`, `jcode-tool-core`, `jcode-tool-types`)
- what: tool implementations incl. communicate (3,364 — likely shell/exec), discover (2,981 — project discovery), todo (2,527); risk-scored commands (`jcode-command-risk`).
- approach: typed tool definitions in own crates; per-tool result shapes.
- deps: protocol/types crates.
- APEX copy: separate tool schema crate; command-risk scoring for approvals.
- APEX improve: a single native tool dispatcher with JSON-schema tool contracts and failure-classification (jcode lacks explicit failure-class taxonomy; APEX needs one).
- APEX reject: tool impls embedded in server modules (hard to reuse).

### Providers (`jcode-base/src/provider/mod.rs` 2,995; `jcode-provider-*`)
- what: metadata + runtime crates per provider (openai, anthropic, gemini, openrouter, copilot, cursor, claude-cli, grok-build, bedrock, antigravity), env-based config, doctor E2E suite.
- why: role-free but provider-diverse; each provider gets a runtime crate so compile-time isolation is preserved.
- approach: `provider-core` traits + runtime crates; `model_resolution` tests (2,435).
- APEX copy: provider trait boundary; per-provider runtime isolation; doctor/E2E provider tests.
- APEX improve: capability roles (FAST/SMART/CODE/DEBUG...) on top of providers; success-rate learning per task class (jcode has no learned routing stats beyond model resolution).
- APEX reject: nothing major — but don't copy the provider count; start with OpenAI-compatible + Anthropic + Gemini.

### Search & edit
- what: `jcode-fuzzy` (fuzzy search), discovery benchmarks; editing is tool-based (communicate/apply via tools); no dedicated multi-strategy edit engine or LSP/DAP integration found in the core (no `lsp/` or `dap/` dirs).
- APEX copy: fuzzy search; discovery tooling.
- APEX improve: add tiered search (literal→regex→symbol→AST→semantic) and a multi-strategy edit engine with LSP validation (both absent here — this is oh-my-pi's strength).
- APEX reject: none — absence is a gap to fill, not a pattern to copy.

### TUI (`jcode-tui`)
- what: ratatui/crossterm client with 28 keybindings, kill-ring editor, split view, session picker, remote server events, usage overlay, mermaid/markdown renderers, permissions UI.
- why: rich terminal UX; the user's own `aimyway-jcode-tui` skill documents the exact port (28 keybindings, 8 right panels, reasoning-block rendering).
- APEX copy: keybinding model, kill ring, session picker, split view, usage overlay (token/context%) — all proven UX.
- APEX improve: keep TUI as a client of the core protocol, not a monolith (jcode-tui files are huge: input.rs 4,084, commands.rs 3,664).
- APEX reject: 4K-line UI files.

### Telemetry (`jcode-telemetry-core`, `telemetry-worker/`)
- what: separate telemetry worker process; event capture.
- APEX copy: out-of-process telemetry; event bus separation (matches APEX event bus goal).
- APEX reject: none — but ensure telemetry is opt-in (jcode's AGENTS.md-era PRs show scrutiny here).

### Sessions & storage (`jcode-storage`, `jcode-session-types`)
- what: session persistence with durable state; `durable_state.rs` in server.
- APEX copy: typed session types; durable state snapshots.
- APEX improve: FTS-indexed session search (Hermes/ai-memory pattern).

### Tests & benchmarks
- what: extensive tests (`live_tests.rs` 3,087 lines — live/PTY tests; provider E2E; TUI tests incl. onboarding eval 3,303); `scripts/benchmark_discovery.py`, `benchmark_attribution.py` (sponsor attribution), `memory_recall_bench.rs` (2,667); benchmark claims on README + jcode.sh/bench.
- **Published claims (repo-maintainer, VERIFIED-UNVERIFIED)**:
  - RAM 1 session: jcode 27.8 MB PSS (embeddings off) vs pi 144.4, Codex 140.0, OpenCode 371.5, Copilot 333.3, Cursor 214.9, Claude Code 386.6, Antigravity 243.7 (README table) — **UNVERIFIED** (no reproduction harness in repo for the PSS numbers; methodology = "measured on this Linux machine").
  - RAM 10 sessions: jcode 117.0 MB (emb off) / 260.8 (emb on) vs Claude Code 2300.6 — **UNVERIFIED**.
  - Time to first frame: jcode 14.0 ms vs pi 590.7, Codex 882.8, OpenCode 1035.9, Claude Code 3436.9 (10 interactive PTY launches) — **UNVERIFIED**; plausible for a native TUI, but must be reproduced independently (APEX benchmark suite requirement).
  - Discovery/attribution benchmarks have in-repo harnesses (`scripts/benchmark_*.py` + case JSON) — **VERIFIED** as existing harnesses (results not re-run here).
- APEX copy: maintainer-benchmark skepticism; in-repo benchmark scripts with case files; PTY first-frame measurement technique.
- APEX improve: publish methodology + scripts with every claim; measure on macOS (jcode measured Linux).

### Failure recovery / security
- what: reload_recovery (daemon crash), provider 413 recovery, `jcode-command-risk` approval scoring, permissions UI, safety.rs (references consolidation — prompt-injection hygiene for memory), telemetry opt-in scrutiny.
- APEX copy: command-risk scoring; 413/payload recovery; reload recovery.
- APEX improve: explicit failure-class taxonomy + strategy-switching retries (APEX §23).
- APEX reject: none major.

## 4. Verdict for APEX

**Copy conceptually (top 5)**
1. Daemon + thin clients (multi-session RAM efficiency; headless `run`).
2. Compaction discipline: threshold ladder, verbatim recent turns, flat image token cost, emergency caps.
3. Memory with relevance-gated top-k injection + optional lazy embeddings + cross-encoder rerank + skills-as-memory.
4. Swarm message hygiene (tldr protocol, typed completion reports, member caps).
5. In-repo benchmark scripts + maintainer-claims skepticism; provider runtime isolation; out-of-process telemetry.

**Weaknesses**
- Tool/comm modules embedded in a giant server crate; 4K-line files.
- No LSP, no DAP, no multi-strategy edit engine, no tiered search (gaps APEX must fill from oh-my-pi).
- Compaction constants hardcoded to one provider's context size.
- Published RAM/TTFB numbers lack in-repo reproduction harnesses.
- Memory sidecar couples to a specific small model.

**Improve**: per-model budgets, learned routing stats, failure-class retries, typed subagent schemas, FTS session search.

**Reject**: 1,000-member swarm target; massive single-file modules; hardcoded provider assumptions.

**Reuse score: 7/10 as architecture reference; 4/10 as fork base.** The crate layout is exemplary for a Rust harness and the compaction/memory/tldr ideas are directly liftable, but jcode is built around a bespoke daemon protocol and giant modules that would need restructuring for APEX's clean-core mandate. Clean reimplementation of its concepts is cheaper than forking.
