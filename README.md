# ByteAi (codename: APEX)

The fastest, smartest autonomous coding agent.

ByteAi is a new open-source autonomous coding-agent harness. It is NOT a merge of
existing projects: it is a clean architectural synthesis built from research into
six reference implementations, adopting their strongest ideas, improving their
weak points, and rejecting their dead weight.

## Install

### One-command (any machine with cargo + git)

```sh
curl -fsSL https://raw.githubusercontent.com/byteai/byteai/main/install.sh | bash
```

Installs to `~/.local/bin/byteai` and writes a default config. Or build manually:

```sh
git clone https://github.com/byteai/byteai && cd byteai
cargo build --release
./target/release/byteai doctor
```

Configure a provider (any OpenAI-compatible endpoint) in
`~/.config/byteai/config.toml` (macOS: `~/Library/Application Support/byteai/config.toml`):

```toml
[[providers]]
name = "bai"
base_url = "https://api.b.ai/v1"
api_key_env = "MY_API_KEY_ENV_VAR"
model = "deepseek-v4-flash"
```

## Reference projects (Phase 0 research subjects)

| Project | Repo | License | Role in APEX |
|---|---|---|---|
| jcode | https://github.com/1jehuang/jcode | MIT | Rust performance core, memory, swarm ideas |
| oh-my-pi | https://github.com/can1357/oh-my-pi | MIT | LSP-aware edits, DAP, kernels, worktree subagents |
| mattpocock/skills | https://github.com/mattpocock/skills | MIT | Engineering discipline, skill format |
| i-have-adhd | https://github.com/ayghri/i-have-adhd | MIT | ADHD-friendly UX rules |
| ai-memory | https://github.com/akitaonrails/ai-memory | MIT | Memory model: Markdown + SQLite + FTS + optional vectors |
| hermes-agent | https://github.com/NousResearch/hermes-agent | MIT | Skills lifecycle, delegation, FTS sessions, MCP |

## Architecture

    FAST CORE + INTELLIGENT ORCHESTRATION + OPTIONAL POWER MODULES

- **Fast core**: Rust. Agent loop, scheduler, provider routing, tool dispatcher,
  filesystem, search, patch/edit engine, session engine, context engine, LSP
  manager, DAP manager, memory router, subagent coordinator, process manager,
  telemetry, TUI protocol.
- **Optional processes**: Python kernel, JS/Bun kernel, embedding service,
  browser service, remote execution workers. The basic agent runs with NO
  Python/Node dependency.

## Performance targets (aspirational, measured not guessed)

- Cold start < 100 ms
- Idle RAM < 50 MB (without embeddings); < 20 MB per additional session
- Verified against jcode / oh-my-pi / Hermes / Claude Code / Codex / OpenCode in `benchmarks/`

## Documentation

- `docs/research/` — per-project research notes + architecture comparison + feature matrix + benchmark methodology
- `docs/adr/` — Architecture Decision Records
- `docs/` — design docs (context engine, memory, security model, ...)

## Status

- [x] Phase 0 — Research (see `docs/research/`)
- [x] Phase 1 — Minimal fast agent (Rust CLI, provider, streaming, tools, sessions, TUI)
- [x] Phase 2 — Code intelligence (LSP, AST, smart search, smart reads, patching)
- [x] Phase 3 — Verification (test detection, typecheck, diagnostics, verification gate)
- [x] Phase 4 — Debugging (DAP)
- [x] Phase 5 — Memory (working state, project wiki, FTS, entities, handoffs)
- [x] Phase 6 — Skills (loader, engineering skills, lesson capture, promotion)
- [x] Phase 7 — Multi-agent (workers, worktrees, structured communication, conflict detection)
- [x] Phase 8 — Smart router (capability-based model routing + learning)
- [x] Phase 9 — Reviewer (independent verification agent)
- [x] Phase 10 — Polish (TUI, agent hub, plugins, MCP, remote execution)

Every phase ends with a benchmark run (`benchmarks/`).

## License

MIT (pending final decision in `docs/adr/0000-record.md`). Research notes
reference the reference projects; no source code is copied without attribution.
