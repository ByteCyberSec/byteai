# i-have-adhd — Research Notes

> Source: https://github.com/ayghri/i-have-adhd (clone at `research/repos/i-have-adhd`, 2026-08-25)
> Document purpose: Phase 0 research for ByteAi (ByteAi). Verified by reading actual files.

## 1. Overview

- **What**: A small plugin pack (56 files, 7,462 LOC) that enforces ADHD-friendly interaction rules on coding agents. The core skill/SKILL.md (140 lines) defines the rules; hooks/always-on.mjs (44 lines) enforce them; extensions/ provide platform-specific implementations; evals/ and tests/ verify compliance.
- **License**: MIT, Copyright (c) 2026 Ayoub Ghriss.
- **Positioning**: Solve the problem that AI agent responses are verbose, preamble-heavy, unstructured, and hard for ADHD users to follow. Enforce: action-first output, numbered steps, no "Great question!" preambles, no repeating the user's request, no giant unstructured paragraphs, make completed wins visible, concrete next action.

## 2. Pack Contents

- `skills/i-have-adhd/SKILL.md` (140 lines) — the core interaction rules
- `hooks/always-on.mjs` (44 lines) + `always-on.sh` (37 lines) — Claude Code hooks that load the skill on every session
- `extensions/i-have-adhd.ts` (222 lines) — OMP/PI platform extension (injects rules into system prompt)
- `extensions/context-compat.ts` (61 lines) — context compatibility checker
- `evals/` (4 files, 122 LOC) — eval runners and example config
- `tests/` (5 files, 468 LOC) — test scripts for hooks, opencode plugin, OMP package
- `scripts/` — `check_pi_extension.py` (371 lines), `check_context_compat.ts` (87 lines), `run_evals.py` (371 lines)
- Plugin manifests: `plugin.json` (Claude), `.codex-plugin/plugin.json`, `.opencode/plugins/i-have-adhd.mjs`, `.cursor/skills/i-have-adhd/SKILL.md`, `gemini-extension.json`, `kimi.plugin.json`, `qwen-extension.json`
- `AGENTS.md` (71 lines), `README.md` (102 lines), `INSTALL.md` (752 lines — multi-language install docs)

## 3. Core Rules (from SKILL.md)

The SKILL.md defines these output rules:

1. **Default final output structure**: 
   - 1. What changed
   - 2. Verification
   - 3. Any blocker/risk
   - 4. Next action
2. **Avoid**: "Great question!", "Certainly!", "Hope this helps!", long preambles, repeating the user's request, giant unstructured paragraphs.
3. **During work**: surface meaningful progress without narrating every trivial tool invocation.
4. **Interactive instructions**: lead with the action, use short numbered steps, keep immediate choices small, make completed wins visible.
5. **Concrete next action**: always end with a clear next step.

## 4. Enforcement Mechanisms

- **hooks/always-on.mjs**: A Claude Code PreToolUse hook that runs every tool call. It loads the `i-have-adhd` skill and adds a system prompt override ensuring the ADHD rules are active.
- **hooks/always-on.sh**: Shell variant of the same hook.
- **extensions/i-have-adhd.ts**: OMP/PI extension — injects the rules into the agent's system prompt on startup. Uses the PI extension API.
- **opencode plugin** (`.opencode/plugins/i-have-adhd.mjs`): OpenCode plugin that enforces the rules.
- **Evals** (`evals/`): run_evals.py checks that the rules are actually being followed by measuring output formats.
- **Tests** (`tests/`): test the hooks, extension, and plugin; verify context compatibility.

**How rules are encoded**: 50% as prompt directives (SKILL.md body text the model reads) and 50% as code (hooks enforce the prompt injection, extensions inject into system prompt). The enforcement is at the platform plugin level, not at the model or harness level — meaning a model can still ignore the rules if it chooses.

## 5. Weaknesses

- No harness-level enforcement: the rules are only as strong as the model's ability to follow prompt instructions.
- No formatting validation: the evals/tests check basic compliance but don't automatically reformat output.
- No structured output (JSON schema) enforcement: the rules are prose, not schema.
- Single-platform hooks: Claude Code hooks differ from OpenCode plugins differ from Gemini extensions — no unified enforcement.
- The SKILL.md is a prompt injection, not a compiled instruction: a determined model can ignore it.

## 6. Verdict for ByteAi

**Copy (exact rules to adopt)**:
1. Default final output format: `1. What changed / 2. Verification / 3. Any blocker/risk / 4. Next action`
2. Prohibited phrases: "Great question!", "Certainly!", "Hope this helps!"
3. No repeating the user's request back to them.
4. No giant unstructured paragraphs — use short sections with clear headers.
5. During work progress: surface meaningful progress without narrating every tool call.
6. Interactive instructions: lead with action, short numbered steps, completed wins visible.

**How ByteAi should encode these rules**:
- **Primary**: as a system-level output format schema (ByteAi's TUI/CLI should enforce formatting at the display layer, not as a prompt instruction). The agent chooses the content; the display layer enforces the structure.
- **Secondary**: the output rules (what changed / verification / blockers / next action) should be embedded in the system prompt as a short, versioned block.
- **Third**: a verification hook should check that the model's final output follows the structure before delivery. If not, reformat it (ByteAi's display layer does this automatically).
- **Reject**: hook-level enforcement that depends on platform-specific plugin APIs. Instead, make the output format a core harness invariant (like role alternation or prompt caching in Hermes).

**Reject from i-have-adhd**:
- "Always" rules that remove legitimate nuance (e.g., "always lead with the action" — sometimes context is needed first; ByteAi should default to action-first but allow context when genuinely necessary).
- Platform-specific hook implementations — ByteAi handles this at the core level.