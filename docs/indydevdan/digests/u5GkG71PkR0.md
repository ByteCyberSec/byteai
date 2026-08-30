# u5GkG71PkR0 — "The Claude Code Feature Senior Engineers KEEP MISSING"

## CORE_THESIS
Validation increases trust in agents, and trust saves engineering time. Claude Code shipped a release most engineers missed: **you can now run hooks inside skills, subagents, and custom slash commands** — enabling *specialized self-validating agents*. Before, hooks were global (settings.json); now each prompt/subagent/skill carries its own deterministic validation. Demonstrated with a personal-finance pipeline: `/review-finances` runs an end-to-end chain of agents (categorize CSV, normalize, merge accounts, generative UI) where every step validates its own work.

## KEY_TECHNIQUES
- **Custom command = prompt + frontmatter hooks**: a `/csvedit` command with `post_tool_use` hook keyed to read/edit/write, pointing at a validator script (`uv run` + pandas CSV parse). Hook script outputs its own log file; on failure it returns `"Resolve this CSV error in <path>"` plus dumped issues, directing the agent to fix — demo shows a deliberately broken CSV being detected and repaired automatically on the next loop.
- **Validator organization**: `.claude/hooks/validators/` directory, one validator per concern, each writing its own log — observability is why everything is logged.
- **Subagents** add parallelization + context isolation: deploy one CSV-edit agent per file in parallel (4 agents, validated within ~1s of each other, provable via logs).
- **Stop hook** for global validators that test all files when an agent finishes; **post-tool-use** for single-file validation (gets the edited path in-scope). E.g., build.md runs a linter + formatter (Astral uv/ruff) only when the build agent runs.
- **Guarantee beats prompting**: putting validation in the hook means it *always* runs — a "closed-loop prompt" that needs no further prompt engineering.
- Power move: pass an entire settings file (including hooks) as JSON to the primary agent via `claude -p --settings`.

## TOOLS_AND_RECOMMENDATIONS
Claude Code hooks (pre/post tool use, stop), skills/subagents/slash-command hooks, validators dir + log files, uv/pandas validators, ruff/lint hooks, `claude --settings` JSON, Opus 4.5. Recommends reading the docs and building focused self-validating agents rather than a generalist "do-everything" omni-agent.

## APPLICABLE_PRINCIPLES
A focused agent with one purpose outperforms an unfocused agent with many end states — and that holds over tens/hundreds/thousands of runs. Self-validation must be hyper-focused on the prompt's purpose. "Agents + code beats agents alone": insert determinism into agent workflows; every good engineer validates their work, so every good specialized agent should too. Anti-vibe-coding: engineers must know what their agents are doing — read the documentation yourself, never delegate learning.
