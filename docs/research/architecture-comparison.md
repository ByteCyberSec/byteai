# APEX (ByteAi) — Architecture Comparison & Decision Matrix

> Phase 0 deliverable. Synthesizes the six research documents in `docs/research/`.
> Date: 2026-08-25

## 1. Executive Summary

APEX should be a **clean Rust implementation** — not a fork of any reference
project. The strongest ideas from each reference are adoptable as concepts and
patterns, but every codebase has structural problems that make forking the
wrong call: jcode's bespoke daemon protocol + giant modules, oh-my-pi's
TypeScript core and Bazel/nix build weight, Hermes' Python runtime (2.8M LOC),
ai-memory's standalone-server design. A clean core in Rust with the
architecture proven by these projects gives APEX the performance mandate
(<100 ms start, <50 MB idle) without inheriting anyone's debt.

## 2. Project-by-project verdict (details in `docs/research/<project>.md`)

| Project | Language | Size (LOC text) | License | Fork score | Role in APEX |
|---|---|---|---|---|---|
| jcode | Rust | ~785K (much is tests) | MIT | 4/10 | Performance patterns: daemon+thin clients, compaction ladder, memory graph + optional embeddings, swarm tldr hygiene, in-repo benchmarks |
| oh-my-pi | Rust natives + TS core | ~3.16M (catalog-heavy) | MIT | 2/10 | Code-intelligence patterns: LSP writethrough, DAP client, multi-strategy edit engine (sloppy grammar), task executor with spawn policy, advisor |
| mattpocock/skills | Markdown | ~9K | MIT | n/a (content pack) | Skill format + engineering discipline content: grilling, diagnosing-bugs, TDD, ADRs, handoff, to-spec |
| i-have-adhd | Markdown + JS | ~7K | MIT | n/a (content pack) | ADHD-friendly UX rules: action-first output, 4-part final format, no preambles |
| ai-memory | Rust | ~232K | MIT | 6/10 (companion) | Memory model: Markdown source of truth + SQLite/FTS5 + optional embeddings + compile-not-retrieve + handoffs + lifecycle hooks (20+ agents) |
| hermes-agent | Python | ~2.8M | MIT | 1/10 | Architecture philosophy: narrow waist, skills lifecycle, delegation API + output_schema, FTS5 session store, security (redaction/approval), prompt-caching invariant |

## 3. Decision Matrix (fork vs core vs clean)

| Criterion | Fork jcode | Fork oh-my-pi | Fork Hermes | Reuse ai-memory | **Clean Rust** |
|---|---|---|---|---|---|
| Performance targets (<100 ms, <50 MB) | ✅ Rust, but daemon+giant modules | ❌ TS core (Bun/Node floor) | ❌ Python | n/a (server) | ✅ by construction |
| Code quality / maintainability | ⚠️ 4K-line files, bespoke protocol | ❌ 3.1M LOC, Bazel+nix | ❌ 10K files, 30K-line modules | ⚠️ 11K-line hook router | ✅ small interfaces, deep modules |
| License compatibility | ✅ MIT | ✅ MIT | ✅ MIT | ✅ MIT | ✅ MIT (attribution only) |
| Reuse potential | ⚠️ concepts yes, code no | ⚠️ subsystems to reimplement | ⚠️ philosophy yes | ✅ memory model + MCP surface | ✅ nothing inherited |
| Architectural fit (fast core + optional modules) | ⚠️ close but coupled | ⚠️ TS orchestration | ❌ monolith core | ✅ companion memory | ✅ designed for it |
| Development speed | ⚠️ fast start, slow at scale | ❌ build system overhead | ⚠️ fast in Python, slow perf | ✅ fast (Rust) | ⚠️ slowest start, fastest end state |
| Startup cost | ✅ | ❌ | ❌ | n/a | ✅ |
| Long-term ownership | ⚠️ upstream churn | ⚠️ upstream churn | ⚠️ upstream churn | ⚠️ companion dep | ✅ full control |

## 4. The APEX Architecture (fast core + intelligent orchestration + optional power modules)

```
byteai/  (codename APEX)
├── crates/
│   ├── apex-core/           agent loop, state machine, scheduler, tool dispatcher
│   ├── apex-provider/       provider traits + OpenAI-compatible/Anthropic/Gemini/Ollama/vLLM runtimes, role router
│   ├── apex-tools/          native tools: shell, fs, read, search, edit, todo, memory, delegate
│   ├── apex-edit/           multi-strategy edit engine (exact/contextual/sloppy/AST/whole-file) + LSP validation
│   ├── apex-lsp/            LSP manager (diagnostics, symbols, refs, rename, formatting, writethrough)
│   ├── apex-dap/            DAP manager (launch/attach/breakpoints/step/evaluate; debugpy/lldb/gdb/dlv/node)
│   ├── apex-search/         tiered search: literal → regex → symbol/LSP → AST → optional semantic
│   ├── apex-read/           progressive-disclosure file reader (symbols, ranges, functions, classes, AST nodes)
│   ├── apex-session/        session engine + FTS5 store (state.db), session search, resume, handoff
│   ├── apex-context/        context budget, compaction (jcode ladder), checkpoint/rewind, eviction
│   ├── apex-memory/         hybrid memory: working state / project wiki / episodic / procedural
│   │                        Markdown source of truth + SQLite indexes (FTS, entities, graph, optional embeddings)
│   ├── apex-skills/         SKILL.md loader, lesson capture, promotion, verification
│   ├── apex-subagent/       subagent coordinator: spawn/steer/stop, worktrees, conflict detection, typed results
│   ├── apex-router/         capability-based model routing with learned per-task-class stats
│   ├── apex-review/         optional independent reviewer (INFO/WARNING/BLOCKER)
│   ├── apex-exec/           execution abstraction: local / Docker / SSH / remote sandbox
│   ├── apex-events/         event bus (session.*, tool.*, file.*, diagnostic.*, test.*, agent.*, memory.*)
│   ├── apex-security/       redaction, approval policies, path restrictions, prompt-injection detection
│   ├── apex-telemetry/      metrics, usage, agent hub data
│   ├── apex-protocol/       typed tool/subagent/result schemas (JSON-RPC-ish, serde)
│   └── apex-tui/            TUI client (ratatui) over the core protocol
├── kernels/                 optional processes: python kernel, js/bun kernel, embedding service, browser service
├── skills/                  bundled skills (matt-pocock engineering content reimplemented)
├── benchmarks/              reproducible benchmark suite (see benchmark-methodology.md)
└── evals/                   quality eval tasks (16 task classes) + ablation harness
```

### Lineage of each subsystem (what APEX copies conceptually from where)

| APEX subsystem | Primary source | Also informed by |
|---|---|---|
| Fast core / narrow waist | hermes "core is a narrow waist" | jcode daemon efficiency |
| Agent state machine (phases) | matt-pocock discipline | oh-my-pi session |
| Context compaction ladder | jcode constants | hermes aux-model summarization |
| Hybrid memory (Markdown+SQLite+FTS+optional vectors) | ai-memory | jcode memory graph, hermes FTS sessions |
| Skills lifecycle | hermes skill_manage | matt-pocock SKILL.md format |
| LSP integration | oh-my-pi lsp/ | — |
| DAP integration | oh-my-pi dap/ | — |
| Multi-strategy edit engine | oh-my-pi edit/ (sloppy) | hashline verification |
| Subagents + typed results + worktrees | hermes delegate_task | jcode swarm tldr, oh-my-pi task executor |
| Capability-based routing | APEX spec §2-3 (not in references) | hermes aux client, oh-my-pi catalog |
| Tiered search / progressive reads | APEX spec §6-7 (not in references) | oh-my-pi natives |
| Event bus | APEX spec §31 | jcode telemetry, hermes hooks |
| Security | hermes redaction/approval | oh-my-pi secrets/cleanse |
| ADHD UX | i-have-adhd rules | — |
| Benchmarks | jcode scripts + oh-my-pi edit-benchmark | — |

## 5. What APEX Rejects

- jcode: 1,000-member swarm target; 4K-line files; hardcoded 200k context budget; bespoke daemon socket protocol (use typed JSON-RPC).
- oh-my-pi: TS core; Bazel/nix/Bun build; 3.1M-LOC monorepo; nvim-mason DAP path discovery; vendored proto surface.
- hermes: Python; 30K-line gateway modules; 866K-LOC test sprawl.
- ai-memory: 162-file per-agent hook system (single clean interface instead); giant files (11K hook router).
- matt-pocock: the "no verification, no failure handling, no versioning" skill format as-is.
- i-have-adhd: platform-specific hook enforcement (APEX enforces output format at the display layer + core invariant).

## 6. Licensing

All six references are MIT. Any code copied verbatim must retain the original
copyright notice (MIT requires it); APEX is being written as a clean
reimplementation of concepts, so no reference code is copied. Where small
portions of prompt text or schemas are adapted (e.g. handoff format,
grill questions, benchmark ideas), an attribution section is maintained in
`NOTICE.md`. APEX itself ships MIT.

## 7. Final Recommendation

**Start as a clean Rust implementation** ("fast core"), adopting concepts from
all six projects per the lineage table. Optionally use ai-memory as a
companion process later for heavy cross-harness memory, but APEX's core memory
module must be embedded (no external process for basic operation — the
performance mandate). This is the only option that satisfies all three pillars:
performance (Rust), correctness discipline (skills/engineering), and
maintainability (small interfaces, deep modules).
