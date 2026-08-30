# 3_mwKbYvbUg — "One Prompt Every AGENTIC Codebase Should Have (For Engineering Teams)"

## CORE_THESIS
You can judge an engineering team by how long it takes a new engineer to run the project locally — great teams get it to "one link, one doc, a few commands"; most teams take 1–2 days of pair programming and stale docs. In the age of agents, install + maintain workflows should be standardized by combining deterministic scripts, logging, and agentic prompts. **Agents + code beats either alone**: deterministic hooks give predictable execution, agentic prompts add intelligent oversight and interactivity. Claude Code shipped a new `Setup` hook (install + maintenance modes) that runs before sessions for exactly this — operations you don't want on every session (dependency installs, migrations, periodic maintenance). The novel move is wrapping that hook with prompts, logging, and docs to make onboarding agent-driven.

## KEY_TECHNIQUES
- **Justfile as launchpad**: a `just` command runner standardizes every workflow/agent launch (frontend setup, backend, reset artifacts, `just cli`, `just clm` maintenance) so humans and agents never re-look-up CLI flags.
- **`--init` flag** triggers the new Setup hook → deterministic `setup-init` / `setup-maintenance` scripts (uv run npm install, SQLite migrations, dependency update).
- **`/prime`** as a dynamic, on-demand CLAUDE.md — pulls docs, understands the codebase, reports how it works.
- **`/install` prompt** with a mode variable: interactive (human-in-the-loop via ask-user-question tool, ~4 questions per batch: database fresh/full, env-var handling, guided setup) vs one-shot fully-agentic install; workflow = /prime → check interactive → read log → write results → report.
- **Logging as observability**: setup.log lets the agent read back and report install success/failures.
- **Common workflow resolution**: encode "problem → solution" pairs (e.g., "database corrupt → clear and rerun") into the prompt so the agent self-resolves; verification steps confirm each step landed.

## TOOLS_AND_RECOMMENDATIONS
Justfile (`just`), Claude Code Setup hook (init + maintenance), `/install`, `/prime`, CLAUDE.md, log files, docs-scraping steps. References Mintlify's Jan 15 "LM executables" post as the same pattern (no standard needed — the idea matters). Recommends deploying this in every codebase, especially for new-hire onboarding, new machines, and agent sandboxes.

## APPLICABLE_PRINCIPLES
Standardize install/maintain as "living documents that execute" — when something changes, you update the script and the prompt, not stale docs. The agentic layer should be consistent and repeatable. Clear ROI framing: onboarding time × team growth rate is the cost you're eliminating. "Write the config prompt, write the maintenance prompt, then walk through it with your agents."
