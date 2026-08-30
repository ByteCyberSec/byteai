# ByteAi Feature Matrix (reference projects vs ByteAi target)

Legend: ✅ = present (verified in source), ⚠️ = partial/limited, ❌ = absent, 🎯 = ByteAi target (this column is the spec, not current implementation)

| Mechanism | jcode | oh-my-pi | mattpocock/skills | i-have-adhd | ai-memory | hermes-agent | 🎯 ByteAi |
|---|---|---|---|---|---|---|---|
| Language / runtime | Rust | Rust natives + TS core | Markdown | Markdown+JS | Rust | Python | Rust core |
| Agent loop (turn → model → tools) | ✅ server.rs/live_turn | ✅ agent-session.ts | ❌ (procedure only) | ❌ | ❌ (memory server) | ✅ conversation_loop.py | ✅ phase state machine |
| Tool registry + schemas | ✅ jcode-tool-core | ✅ tools/ + omptype | ❌ | ❌ | ❌ (MCP tools) | ✅ tools/ + toolset | ✅ typed serde schemas |
| Tool dispatch / dispatcher | ✅ | ✅ | ❌ | ❌ | ❌ | ✅ | ✅ native dispatcher |
| Prompts as static files | ⚠️ | ✅ (AGENTS.md rule) | ✅ (skills are prompts) | ✅ (SKILL.md) | ⚠️ | ⚠️ | ✅ versioned .md |
| Context compaction | ✅ threshold ladder | ⚠️ compress/ | ❌ | ❌ | ❌ | ✅ aux-model summarizer | ✅ ladder + aux summarize |
| Context budget | ✅ | ⚠️ | ❌ | ❌ | ❌ | ⚠️ | ✅ |
| Checkpoint / rewind | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | 🎯 (spec §25) |
| Memory: working state | ⚠️ pending/injected | ⚠️ memories/ | ❌ | ❌ | ✅ observations | ✅ user profile + notes | ✅ |
| Memory: project knowledge | ✅ memory graph | ⚠️ | ✅ CONTEXT.md | ❌ | ✅ wiki pages | ⚠️ | ✅ |
| Memory: episodic | ✅ | ⚠️ hindsight | ✅ handoff | ❌ | ✅ sessions/observations | ✅ session DB | ✅ |
| Memory: procedural/skills | ✅ skills-as-memory | ⚠️ autolearn | ✅ skills pack | ❌ | ❌ | ✅ SKILL.md lifecycle | ✅ |
| Memory: Markdown source of truth | ❌ (internal) | ❌ | ✅ (skills only) | ❌ | ✅ wiki in git | ❌ (SQLite only) | ✅ |
| Memory: SQLite index | ⚠️ | ⚠️ | ❌ | ❌ | ✅ FTS5+entities+links | ✅ state.db FTS5 | ✅ |
| Memory: embeddings | ✅ optional (ONNX) | ❌ | ❌ | ❌ | ✅ optional (providers) | ❌ | ✅ optional |
| Memory: conflict resolution | ⚠️ | ❌ | ❌ | ❌ | ⚠️ (implicit) | ⚠️ | 🎯 (spec §15) |
| Session storage | ✅ jcode-storage | ✅ session/ | ❌ | ❌ | ✅ | ✅ state.db | ✅ |
| Session search (FTS) | ⚠️ | ⚠️ | ❌ | ❌ | ✅ | ✅ session_search | ✅ |
| Session handoff | ⚠️ | ⚠️ | ✅ handoff skill | ❌ | ✅ auto handoffs | ⚠️ | ✅ auto at session end |
| Skills: SKILL.md format | ✅ | ⚠️ | ✅ | ✅ | ❌ | ✅ | ✅ |
| Skills: lifecycle (candidate→promote) | ⚠️ | ⚠️ autolearn | ❌ | ❌ | ⚠️ auto-improve | ✅ | ✅ |
| LSP integration | ❌ | ✅ client+writethrough | ❌ | ❌ | ❌ | ❌ | ✅ |
| DAP / debugger | ❌ | ✅ client (gdb/lldb/js) | ❌ | ❌ | ❌ | ❌ | ✅ |
| Persistent kernels (python/js) | ❌ | ✅ robomp + eval worker | ❌ | ❌ | ❌ | ❌ | 🎯 optional modules |
| Edit engine: multi-strategy | ⚠️ | ✅ sloppy+replace+hashline | ❌ | ❌ | ❌ | ⚠️ | ✅ |
| Edit validation (LSP/typecheck after) | ❌ | ✅ (writethrough) | ❌ | ❌ | ❌ | ⚠️ | ✅ |
| Search: literal | ✅ | ✅ | ❌ | ❌ | ❌ | ✅ | ✅ |
| Search: regex | ✅ | ✅ Rust grep | ❌ | ❌ | ❌ | ✅ | ✅ |
| Search: symbol/LSP | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Search: AST | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | 🎯 |
| Search: semantic | ✅ embeddings | ❌ | ❌ | ❌ | ✅ optional | ❌ | ✅ optional |
| Progressive file reading | ⚠️ | ⚠️ grouped-file-output | ❌ | ❌ | ❌ | ⚠️ | ✅ |
| Subagents / delegation | ✅ swarm | ✅ task/parallel | ❌ | ❌ | ❌ | ✅ delegate_task | ✅ |
| Subagent typed results | ⚠️ tldr protocol | ✅ | ❌ | ❌ | ❌ | ✅ output_schema | ✅ queryable schemas |
| Worktree isolation | ⚠️ | ⚠️ hindsight/git | ❌ | ❌ | ❌ | ✅ -w | ✅ + conflict detection |
| Advisor / reviewer | ❌ | ✅ advisor/ | ✅ code-review | ❌ | ❌ | ❌ | ✅ optional, severity-graded |
| Provider abstraction | ✅ per-provider runtimes | ✅ ai/providers | ❌ | ❌ | ✅ LLM providers | ✅ 20+ providers | ✅ |
| Role-based model routing | ❌ | ⚠️ catalog | ❌ | ❌ | ❌ | ❌ (user-selected) | 🎯 |
| Learned routing stats | ⚠️ model resolution | ❌ | ❌ | ❌ | ❌ | ❌ | 🎯 (spec §3) |
| TUI | ✅ ratatui | ✅ tui/ | ❌ | ❌ | ✅ web UI | ✅ Ink TUI | ✅ ratatui client |
| Agent hub (observe/steer workers) | ⚠️ | ✅ agent-hub.ts | ❌ | ❌ | ❌ | ⚠️ dashboard | ✅ |
| Event bus | ⚠️ telemetry | ⚠️ | ❌ | ❌ | ✅ hooks | ✅ hooks | ✅ |
| Hooks | ✅ | ⚠️ | ✅ guardrails skill | ✅ always-on | ✅ lifecycle hooks | ✅ PreToolUse | ✅ |
| MCP | ❌ | ✅ mcp/ | ❌ | ❌ | ✅ server | ✅ catalog | ✅ external-only |
| Plugins | ⚠️ | ✅ extensibility/ | ❌ | ✅ platform plugins | ❌ | ✅ plugins/ | ✅ |
| Execution envs (docker/ssh) | ⚠️ | ✅ ssh/ + exec/ | ❌ | ❌ | ❌ | ✅ | 🎯 |
| Git awareness | ✅ AGENT_NATIVE_VCS | ✅ utils/git + jj | ✅ guardrails | ❌ | ✅ git2 wiki | ⚠️ | ✅ |
| Test selection (dependency-aware) | ❌ | ⚠️ | ✅ tdd | ❌ | ❌ | ❌ | 🎯 |
| Verification gate | ⚠️ | ⚠️ | ❌ | ❌ | ❌ | ⚠️ | ✅ |
| Failure recovery / retry classes | ⚠️ 413 recovery | ✅ retry-fallback | ❌ | ❌ | ❌ | ✅ TurnRetryState | ✅ failure classes |
| Security: secret redaction | ⚠️ | ✅ secrets-obfuscator | ❌ | ❌ | ✅ sanitized capture | ✅ redact | ✅ |
| Security: approval policies | ✅ command-risk | ⚠️ | ✅ guardrails | ❌ | ❌ | ✅ approval.py | ✅ |
| Prompt-injection defense | ⚠️ safety.rs | ⚠️ cleanse | ❌ | ❌ | ✅ untrusted-memory rules | ⚠️ | ✅ |
| ADHD-friendly UX | ⚠️ | ⚠️ | ❌ | ✅ | ❌ | ⚠️ | ✅ |
| Telemetry | ✅ telemetry-worker | ✅ telemetry-export | ❌ | ❌ | ❌ | ⚠️ | ✅ opt-in |
| Benchmarks in-repo | ✅ scripts/ | ✅ edit-benchmark | ❌ | ✅ evals | ✅ evals | ✅ evals | ✅ full suite |
| RAM/startup targets | ✅ 27.8MB/14ms (claimed) | ⚠️ Bun floor | n/a | n/a | ⚠️ server | ❌ Python | ✅ <100ms/<50MB |
