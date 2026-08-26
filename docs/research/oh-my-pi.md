# oh-my-pi — Research Notes

> Source: https://github.com/can1357/oh-my-pi (clone at `research/repos/oh-my-pi`, 2026-08-25)
> Document purpose: Phase 0 research for APEX (ByteAi). Verified by reading source layout + key files; this is a 3.1M-LOC monorepo, so the analysis focuses on architectural surface, verified by targeted reads.

## 1. Overview

- **What**: The successor of the original "PI" agent — a full IDE-grade coding agent: Rust performance natives + TypeScript agent core + TUI, with LSP, DAP, kernels, subagents, advisor, memory, multi-provider routing, MCP, SSH, browser control, telemetry, and an agent hub.
- **Language**: Rust (crates/pi-* natives: shell builtins, grep, walker) + TypeScript/Bun (packages/: ai, agent, coding-agent, tui, catalog, natives, omptype, utils, stats, wire, hashline, snapcompact, metaharness, mnemopi, browser-relay, collab-web, typescript-edit-benchmark). Python (`python/robomp`) for a hosted tools kernel. Build: Bazel + nix + Bun.
- **Size**: 6,453 text files, ~3.16M LOC (models.json alone is 312K lines — the bundled model catalog; tokenizer fixtures ~270K more). Real code (excluding catalog/tokenizers): ~2.5M LOC.
- **License**: MIT. Copyright (c) 2025 Mario Zechner, (c) 2025-2026 Can Bölük, (c) 2026 Stencil Labs, Inc. THIRD-PARTY-NOTICES.txt (1.0 MB) — substantial third-party surface (vendored proto files, tokenizers).
- **Positioning**: an omni-provider coding agent with native code intelligence (LSP/DAP) and heavy engineering around edit correctness, memory, and multi-agent work.

## 2. Architecture

```
oh-my-pi/
├── crates/
│   ├── pi-natives        — Rust perf-critical: grep.rs (3,282), sed.rs (9,635!), sort.rs (5,732), find.rs (5,416), ls.rs (5,279), tail.rs (3,845); tokenizer fixtures (cl100k/o200k, deepseek-v3)
│   ├── pi-shell          — Rust shell.rs (5,669) — native shell implementation
│   └── pi-walker         — Rust workspace walker (4,927)
├── packages/
│   ├── ai                — multi-provider LLM client: providers/{anthropic.ts 4,399, openai-compat.ts 6,769, cursor.ts 5,413, openai-codex-responses.ts 4,872, openai-responses-wire.ts 6,416, gitlab-duo-workflow-provider.ts, devin/}, auth-storage.ts (6,893), openai-shared.ts (3,834)
│   ├── catalog           — model catalog: models.json (312K lines, ~557 models), provider-models/openai-compat.ts, discovery/cursor-proto.ts (8,329)
│   ├── agent             — agent runtime with tool calling (agent-loop tests 5,158)
│   ├── coding-agent      — the main CLI application (primary focus)
│   ├── tui               — terminal UI library with differential rendering (markdown.ts 3,679, editor.ts 3,667)
│   ├── omptype           — ArkType-compatible schema validation, lazy JIT
│   ├── natives           — bindings to pi-natives (text/image/grep)
│   ├── stats             — local observability dashboard (`omp stats`)
│   ├── hashline          — line-hash verification (edit correctness!)
│   ├── snapcompact       — snapshot/compaction
│   ├── typescript-edit-benchmark — cross-model edit benchmark (all_models_results.json)
│   ├── metaharness       — harness tooling
│   ├── mnemopi           — memory
│   ├── wire / utils / collab-web / browser-relay
└── python/robomp         — Python kernel/host tools (tests/test_host_tools.py 4,945)
```

**coding-agent core dirs** (all under `packages/coding-agent/src/`): `session/` (agent-session.ts 9,831, session-maintenance.ts 4,028), `task/` (executor.ts 3,590, parallel.ts, spawn-policy.ts, provider-concurrency.ts, agents.ts, types.ts, label.ts, repair-args.ts), `edit/` (sloppy.ts 3,973, diff, blackbox, modes/replace, normalize, read-file, renderer, result — multi-strategy), `lsp/` (client, diagnostics, servers, tool, writethrough, types), `dap/` (client.ts 1,043, session.ts, types.ts, config.ts), `advisor/` (advise-tool, runtime, transcript-recorder, delta-split, loop-guard), `compress/` (session.ts), `memories/` + `memory-backend/` + `mnemopi/`, `secrets/`, `security/`, `cleanse/` (agent.ts), `autoresearch/` (index, git, tools/log-experiment), `autolearn/`, `hindsight/` (bank.ts, backend.ts, client.ts, content.ts, mental-models.ts, seeds.json, state.ts, transcript.ts), `commit/` (agentic/agent.ts, tools/analyze-file.ts), `exec/` (non-interactive-env), `subprocess/` (worker-client.ts, worker-runtime.ts), `tools/` (index, debug.ts, eval.ts, computer.ts, gh.ts, gh-pr-checkout.ts, browser/attach, acp-bridge.ts, file-write-fallback.ts, grouped-file-output.ts, fs-cache-invalidation.ts, plan-mode-guard.ts, renderers, tool-timeouts, xdev.ts, builtin-names), `modes/` (interactive-mode.ts 5,773, components/agent-hub.ts, utils/hotkeys-markdown), `mcp/`, `ssh/`, `web/`, `jsonrpc/` (message-framing), `cli/` (gallery-fixtures incl. codeintel), `prompts/` (static .md files — AGENTS.md: "never build prompts in code"), `system-prompt.ts`, `config/` (settings-schema.ts 6,282), `plan-mode/`, `capability/`, `goals/`, `live/`, `tiny/` (tiny inference worker!), `stt/`, `tts/`, `vibe/`, `auto-thinking/` (thinking.ts), `startup-splash.ts`, `sdk.ts`, `main.ts`, `index.ts`, `priority.json`, `workspace-tree.ts`, `telemetry-export.ts` + `telemetry-export-otlp.ts`.

## 3. Mechanism Analysis

### Agent loop (`session/agent-session.ts` 9,831; `packages/agent` runtime)
- what: full agent session state machine: turns, tool calls, retry/fallback (test `agent-session-retry-fallback.test.ts` 5,408), maintenance (`session-maintenance.ts` 4,028), streaming, worker dispatch (CLI re-enters `cli.ts` via `workerHostEntry()` with hidden argv selectors for stats/tab/js-eval/tiny-inference workers).
- why: single well-tested session object; retry/fallback is first-class.
- approach: TS classes with `#private` fields; `Promise.withResolvers`; prompts in static `.md` files imported as text — never built in code (AGENTS.md rule).
- APEX copy: prompts-as-static-files (versioned, testable); worker re-entry pattern for worker processes; retry-fallback as a tested unit.
- APEX improve: APEX's Rust core should keep the loop smaller than this 9.8K-line session class; split into phases (UNDERSTANDING/INVESTIGATING/IMPLEMENTING/VERIFYING/RECOVERING) with tool policies per phase (APEX §45).

### Edit engine (`edit/` — sloppy.ts 3,973 + diff, blackbox, modes/replace, normalize, read-file, renderer, result)
- what: multi-strategy editing: "sloppy" format (a Lark-grammar DSL `sloppy.lark` for loose model-written edit payloads), plus strict replace modes with levenshtein fallback, line-ending/unicode/BOM normalization, diff generation, LSP writethrough (`routeWriteThroughBridge`, `LspBatchRequest`), fs-cache invalidation after writes, plan-mode guard. `hashline` package = line-hash verification of applied edits.
- why: models write sloppy edit payloads; a tolerant grammar + normalization + post-apply verification (LSP + hashes) maximizes edit success.
- approach: pure text transformers (no I/O in the sloppy variants — testable); results aggregated with per-file details; `typescript-edit-benchmark` package ships cross-model edit results (all_models_results.json).
- APEX copy: sloppy-grammar edit format; multi-strategy fallback (exact → contextual → sloppy → whole-file); line-ending/unicode normalization; LSP writethrough; edit-result aggregation; per-model edit benchmarks.
- APEX improve: explicit edit-failure classification + strategy switching (never repeat the same failed method 3× — APEX §8); syntax/LSP validation gating after every edit.

### LSP (`lsp/` — client, diagnostics, servers, tool, writethrough)
- what: real LSP client: server detection/warmup with parallel connect + timeouts (`warmupLspServers`, `LSP_READONLY_ACTIONS`), diagnostics (`FileDiagnosticsResult`), formatting, writethrough batching (`createLspWritethrough`, `flushLspWritethroughBatch`), LSP-backed tool (`LspTool`); regression tests (`tools/lsp-regressions.test.ts` 4,832).
- why: edits validated against real language servers; writethrough keeps the buffer and server in sync.
- APEX copy: LSP client with warmup, diagnostics gating, writethrough; read-only vs mutating action split.
- APEX improve: make LSP optional/async (warmup in background; degrade gracefully when servers missing); cache diagnostics per file with invalidation on change (APEX §27).

### DAP (`dap/` — client.ts 1,043, session.ts, types.ts, config.ts)
- what: full Debug Adapter Protocol client: adapter resolution (DEFAULT_ADAPTERS incl. gdb, lldb-dap, js-debug-adapter via nvim-mason path discovery; `EXTENSIONLESS_DEBUGGER_ORDER = [gdb, lldb-dap]`), JSON-RPC message framing, socket-mode (unix/TCP) with ready timeouts, tool-level debug integration (`tools/debug.ts`), capabilities/state machine.
- why: real debugger control (breakpoints, step, evaluate) instead of print-debugging.
- APEX copy: DAP client architecture; adapter resolution + extensionless order; message framing; tool-timeout integration.
- APEX improve: APEX wants the full debug loop (REPRODUCE→OBSERVE→HYPOTHESIZE→INSTRUMENT→TEST→ROOT CAUSE→MINIMAL FIX→VERIFY→REGRESSION) — oh-my-pi provides the transport, APEX must add the reasoning loop and debugpy/dlv support explicitly.
- APEX reject: dependence on nvim-mason paths (fragile) — probe adapters independently.

### Subagents & parallel execution (`task/` — executor, parallel, spawn-policy, provider-concurrency, agents; `subprocess/` workers)
- what: typed task executor with parallel spawning, spawn policy (concurrency limits), provider-concurrency tracking, agent roles (`task/agents.ts`), label/repair-args; worker processes via CLI re-entry (hidden argv selectors).
- why: bounded parallel work with provider limits prevents rate-limit storms.
- APEX copy: spawn-policy + provider-concurrency; typed task results; worker re-entry.
- APEX improve: isolated git worktrees per worker with conflict detection (oh-my-pi's worktree use is mostly in autoresearch/git.ts and hindsight; APEX must make worktree isolation a first-class coordinator concern — APEX §10); structured result schemas per role (status/findings/files/tests/risks) — oh-my-pi is typed but APEX wants queryable schemas (§11).

### Advisor / reviewer (`advisor/` — advise-tool, runtime, transcript-recorder, delta-split, loop-guard)
- what: independent advisor agent: records transcript, splits deltas, guards loops; tests (`advisor/advisor.test.ts` 5,754). Adversarial verification wired into commit/cleanse agents too (analyze-file, cleanse agent).
- why: catches what the main loop misses; loop-guard prevents advisor feedback loops.
- APEX copy: advisor with transcript + delta-split; loop guard; severity-classified findings (APEX wants INFO/WARNING/BLOCKER).
- APEX improve: make reviewer optional per APEX §12 (not on every turn); concise output contract.

### Memory (`memories/`, `memory-backend/`, `mnemopi/`, `hindsight/`)
- what: multiple memory subsystems: memories/ + memory-backend (persistent backends), mnemopi (memory engine), hindsight (worktree/transcript bank with mental-models, seeds.json, state.ts, backend.ts).
- why: layered: bank (worktrees/state) + memory backends + mental models.
- APEX copy: backend abstraction for memory; hindsight-style state bank.
- APEX improve: adopt ai-memory's Markdown-source-of-truth + SQLite/FTS + optional embeddings discipline instead of bespoke backends; explicit conflict metadata (APEX §15).

### Providers & routing (`packages/ai`, `packages/catalog`)
- what: broad provider surface: anthropic, openai-compat, openai-responses, cursor (with proto), gitlab-duo, devin; auth-storage with credential selection (tests: auth-storage-codex-selection); catalog with ~557-model models.json and provider descriptors.
- APEX copy: provider abstraction; auth-storage; catalog-driven model metadata.
- APEX improve: role-based routing (FAST/SMART/CODE/DEBUG/REVIEW/...) with learned per-task-class success stats (APEX §3) — oh-my-pi routes by provider/model but not by learned capability; cheapest-sufficient-model selection.

### Search & files (`crates/pi-natives`, `packages/natives`, `workspace-tree.ts`, `tools/grouped-file-output.ts`)
- what: Rust-native grep/sed/find/ls/tail/sort; walker; workspace tree; grouped file output (concise regions, not whole files).
- why: native speed + bounded output.
- APEX copy: Rust grep/find natives; grouped/tiered file output; walker.
- APEX improve: tiered search router (literal → regex → symbol/LSP → AST → semantic) choosing cheapest sufficient (APEX §6).

### TUI (`packages/tui`, `modes/interactive-mode.ts` 5,773, `components/agent-hub.ts`, `startup-splash.ts`)
- what: TUI library with differential rendering, markdown + editor components; interactive mode; agent hub component (inspect/steer workers); splash. The original PI agent (which the user's `tui-pi-agent` skill ports exactly: 18 keybindings, ModelSelector, ConfigSelector, @work lazy loading, 0.12s startup) was the earlier generation; this monorepo is v2+.
- APEX copy: differential-rendering TUI; agent-hub component (APEX §30); lazy-loading discipline (0.12s claim from the PI generation — re-verify).
- APEX improve: TUI as a thin client over the core protocol (APEX's Rust core should own the loop; TUI renders events).

### Security (`secrets/`, `security/`, `cleanse/`, `tools/secrets-obfuscator` tests 3,645, `exec/non-interactive-env`)
- what: secrets obfuscation (dedicated test suite), cleanse agent (sanitization), security module, non-interactive env for child processes.
- APEX copy: secrets obfuscator; non-interactive env filtering (APEX §33); cleanse patterns.
- APEX improve: prompt-injection detection on repo content/tool output (APEX §33); path restrictions; approval policies.

### Compression (`compress/session.ts`)
- what: session compression (LSP-aware — grep hit in compress/session.ts).
- APEX copy: compression triggers; keep recent turns.
- APEX improve: adopt jcode's threshold ladder + Hermes' aux-model summarization pattern.

### Benchmarking (`typescript-edit-benchmark`, `catalog` tests, `tests/`)
- what: cross-model edit benchmark package with results JSON; heavy test suites (agent-loop, retry-fallback, advisor, lsp-regressions, secrets-obfuscator, tools, acp-agent 3,311).
- **Published claims**: the 0.12s-startup / 557-model claims trace to the PI-generation README/skills; in this repo, verify via `--smoke-test` (documented in AGENTS.md) — **PARTIALLY VERIFIED**: smoke tests exist; startup numbers must be reproduced.
- APEX copy: edit-benchmark-with-results methodology; smoke-test contract for workers.

### Failure recovery
- what: retry-fallback tests; tool-timeouts; repair-args (repair malformed tool args); session maintenance.
- APEX copy: tool-arg repair; timeout discipline.
- APEX improve: failure-class taxonomy + changed-strategy retries (APEX §23).

## 4. Verdict for APEX

**Copy conceptually (top 5)**
1. LSP-first editing with writethrough + diagnostics gating (the strongest "IDE-grade" pattern among all references).
2. Multi-strategy edit engine: sloppy Lark grammar + replace/levenshtein fallbacks + normalization + line-hash verification + cross-model edit benchmarks.
3. Rust natives for hot paths (grep/sed/find/shell) + TypeScript orchestration boundary — validates APEX's Rust-core + optional-modules split (APEX chooses Rust for the core loop AND these natives; orchestration stays in Rust, not TS).
4. DAP client with adapter resolution + tool integration.
5. Task executor with spawn-policy/provider-concurrency + advisor with loop-guard; prompts as static versioned .md files.

**Weaknesses**
- Monorepo scale (3.1M LOC) is unmaintainable as a fork target; Bazel+nix+Bun build complexity.
- Giant session class (9.8K) and generated/provider surface (proto vendoring, THIRD-PARTY-NOTICES 1MB).
- Learned routing stats absent; subagent worktree isolation not first-class.
- Rust crates (pi-natives) are *natives* for TS, not a standalone core — the loop lives in TS, so RAM/startup floor is Bun/Node-level, not Rust-level.

**Improve**: worktree isolation + conflict detection; learned capability routing; failure-class retries; prompt-injection detection; per-model context budgets.

**Reject**: fork (build system weight, TS core); vendored proto surface; nvim-mason path discovery.

**Reuse score: 8/10 as design reference (LSP/DAP/edit/task subsystems); 2/10 as fork base.** APEX should reimplement the LSP/DAP/edit/task patterns in Rust, not adopt the code.
