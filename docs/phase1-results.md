# Phase 1 Results — Minimal Fast Agent (2026-08-25)

## Delivered

`byteai` release binary (8.6 MB) at `target/release/byteai`. Workspace: 5 crates
(apex-types, apex-provider, apex-tools, apex-core, apex-cli).

| Capability | Status |
|---|---|
| Rust CLI (clap) | ✅ `chat` (one-shot/REPL), `session`, `doctor`, `models`, `tui` |
| Provider abstraction | ✅ OpenAI-compatible streaming (SSE, hand-parsed) |
| Providers tested | ✅ api.b.ai (42 models), OmniRoute :20128 (646 models — models OK, completions 401 pending key registration) |
| Streaming | ✅ content + reasoning_content + tool_call deltas + usage |
| Tools | ✅ shell, read (line-ranged), search (literal+regex), edit (exact-match w/ uniqueness), todo, note |
| Agent loop | ✅ model → tool dispatch → loop; phase state machine (UNDERSTANDING…BLOCKED) |
| Failure classification | ✅ coarse (AUTH/RATE_LIMIT/TIMEOUT/NETWORK/REQUEST/UNKNOWN) |
| Context budget | ✅ 200K budget, jcode-derived eviction (keep last 12 msgs) |
| Sessions | ✅ save/load/list (JSON), resume |
| REPL | ✅ /help /model /new /usage /quit |
| TUI | ✅ minimal ratatui (feature `tui`, default on) |
| ADHD output format | ✅ system prompt enforces What changed / Verification / Blockers / Next action |
| Tests | ✅ 2/2 apex-types wire-format tests |

## Measured (macOS 14.5, Apple Silicon, release build)

| Metric | Result |
|---|---|
| Cold start (`--version`) | ~0.00 s wall, 2.83 MB max RSS (10 runs) |
| Task peak memory (`/usr/bin/time -l`) | 1.88 MB |
| One-shot tool task (create+verify file) | 4.0 s end-to-end, 2 iterations, 1 tool call, 1,968 tokens |
| One-shot trivial task | 1,258 tokens, 1 iteration |
| REPL turn | ~1,241 tokens/iteration |

Reference: jcode claims 27.8 MB PSS / 14 ms TTFB (maintainer benchmark, unverified).
ByteAi's ~2-3 MB RSS is an order of magnitude below that claim — expected for a
single binary with no daemon.

## Known issues

- OmniRoute `/v1/chat/completions` returns 401 with the key that works on
  `/v1/models` — key likely needs registration in OmniRoute's own store.
- Tools execute with `call_id` stamped by the core (providers reject empty).
- Compaction is minimal (drop-oldest); full ladder + aux summarization in Phase 2.

## Next phase

Phase 2 — Code intelligence: LSP manager, AST-aware reads, tiered search
(symbol tier), multi-strategy edit engine with validation.
