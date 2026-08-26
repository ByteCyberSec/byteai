# ByteAi (APEX) — MIT license.
# Third-party material reused with attribution (ADR-0010).

## Reference research repositories (architecture synthesis, all MIT)

| Project | Repo | What was adopted |
|---|---|---|
| jcode | https://github.com/1jehuang/jcode | Rust performance core, memory, swarm ideas |
| oh-my-pi | https://github.com/can1357/oh-my-pi | LSP-aware edits, DAP, kernels, worktree subagents |
| mattpocock/skills | https://github.com/mattpocock/skills | Engineering discipline, SKILL.md format |
| i-have-adhd | https://github.com/ayghri/i-have-adhd | ADHD-friendly UX rules |
| ai-memory | https://github.com/akitaonrails/ai-memory | Memory model: Markdown + SQLite + FTS + optional vectors |
| hermes-agent | https://github.com/NousResearch/hermes-agent | Skills lifecycle, delegation, FTS sessions, MCP |

## Inspirations (patterns, not code)

- deepseek-harness (https://github.com/deepseek-ai/deepseek-harness, MIT) —
  "everything is a plugin" architecture; ByteAi's `plugin` tool follows this.
- claude-mem — progressive session→memory disclosure; ByteAi's `session capture`
  follows this pattern.
- anthropics/skills, obra/superpowers, awesome-agent-skills — SKILL.md standard
  and skill-install-from-GitHub pattern.
- firecrawl (https://github.com/firecrawl/firecrawl, MIT) — web scraping for AI
  agents; ByteAi's `fetch` tool follows this pattern (URL → clean text).
- graphify (https://github.com/Graphify-Labs/graphify, MIT) — local deterministic
  AST knowledge graph; ByteAi's `graph` tool follows this pattern.
- planning-with-files (https://github.com/OthmanAdi/planning-with-files, MIT) —
  persistent file-based planning with completion gate; ByteAi's `plan` tool
  follows this pattern.
- ponytail (https://github.com/DietrichGebert/ponytail, MIT) — YAGNI/over-engineering
  check; ByteAi's `review` tool includes ponytail-inspired advisory checks.
- oh-my-openagent (https://github.com/code-yeongyu/oh-my-openagent, MIT) —
  agent harness for complex codebases; ByteAi's architecture draws from its
  build/plan agent and lazy-context-loading ideas.
- ECC (https://github.com/affaan-m/ECC, MIT) — agent harness performance
  optimization; ByteAi's eval-driven development and review gate are inspired
  by ECC's instincts system.

## Rust crates

All dependencies are pulled from crates.io under their own licenses (MIT/Apache-2.0).
