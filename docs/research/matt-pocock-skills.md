# mattpocock/skills — Research Notes

> Source: https://github.com/mattpocock/skills (clone at `research/repos/mattpocock-skills`, 2026-08-25)
> Document purpose: Phase 0 research for APEX (ByteAi). Verified by reading actual skill files.

## 1. Overview

- **What**: A pack of 105 Markdown skills encoding engineering discipline (not "vibe coding") — requirements grilling, domain modeling, ADRs, TDD, bug diagnosis, handoff, code review, spec-to-tickets, and more. Each skill is a short SKILL.md with YAML frontmatter + markdown body.
- **Language**: Markdown (no runtime harness — skills are ingested by the harness as prompt/instruction material). `package.json` for npm packaging. `scripts/` for installation helpers.
- **Size**: 105 skill files across skills/ (4,415 LOC), 25 docs/ files (2,110 LOC), 9 root files (2,010 LOC). Total: 164 text files, 9,117 LOC.
- **License**: MIT, Copyright (c) 2026 Matt Pocock.
- **Positioning**: Not a harness — a "procedure library" that teaches a harness (Claude Code, specifically) to think like a disciplined engineer. The CLAUDE.md + AGENTS.md integration makes the skills load automatically.

## 2. Skill Inventory

**Engineering** (skills/engineering/):
- `ask-matt` — Router skill: asks which skill or flow fits the situation
- `code-review` — Review changes since a fixed point (commit, branch, tag)
- `codebase-design` — Shared vocabulary for designing deep modules
- `diagnosing-bugs` — Diagnosis loop for hard bugs and performance regressions
- `domain-modeling` — Build and sharpen a project's domain model
- `grill-me` — A relentless interview to sharpen a plan or design
- `grill-with-docs` — Docs-anchored grilling: challenges a plan against living docs
- `grilling` — Grill the user about a plan, decision, or idea
- `handoff` — Compact the current conversation into a handoff document
- `implement` — Implement a piece of work based on a spec or set of tickets
- `improve-codebase-architecture` — Scan for deepening opportunities
- `prototype` — Build a throwaway prototype to answer a design question
- `setup-matt-pocock-skills` — Configure repo for these skills
- `tdd` — Test-driven development (red-green-refactor)
- `to-spec` — Turn the current conversation into a spec
- `to-tickets` — Break a plan, spec, or conversation into tickets
- `triage` — Move issues and PRs through a state machine
- `wayfinder` — Plan a huge chunk of work exceeding one session
- `wizard` — Generate an interactive bash wizard

**Productivity** (skills/productivity/):
- `teach` — Teach the user a skill or concept
- `writing-for-agents` — Writing documents for agents
- `writing-beats` — Writing, exploit: assemble raw material into a journey
- `writing-fragments` — Writing, explore: mine raw fragments, no structure yet
- `writing-shape` — Writing, exploit: shape raw material into an article

**Misc** (skills/misc/):
- `git-guardrails-claude-code` — Set up Claude Code hooks to block dangerous git commands
- `migrate-to-shoehorn` — Migrate test files from `as` type assertions
- `scaffold-exercises` — Create exercise directory structures
- `setup-pre-commit` — Set up Husky pre-commit hooks

**In-progress** (skills/in-progress/):
- `setup-ts-deep-modules` — Wire dependency-cruiser; skill incomplete
- `writing-fragments/shape` — WIP writing skills

## 3. Skill Format Analysis

Every SKILL.md follows this structure (YAML frontmatter + markdown body):

```yaml
---
name: diagnosing-bugs
description: Diagnosis loop for hard bugs and performance regressions.
---
# diagnosing-bugs

## When to Use
When encountering a bug, test failure, or unexpected behavior — especially
if the root cause is not obvious.

## Procedure
1. Reproduce the bug
2. ...
```

Key observations:
- **Frontmatter**: `name`, `description` (57-char first line for the skill index — the pack's README says "long descriptions are truncated to the first 57 chars plus '...'").
- **Body length**: 80-140 lines per skill — compact, focused, no bloat.
- **Trigger discipline**: "When to Use" section is the trigger — the harness should load the skill when the condition is met.
- **Procedure steps**: numbered, small, verified steps.
- **No instructions on HOW to load**: skills assume Claude Code's skill system (or `.claude-plugin/`).
- **No verification section**: skills assume the user/agent verifies independently.
- **No failure handling**: skills assume the procedure works.

## 4. Key Skills Deep Dive

### Grill / Grill-with-docs / Grilling
- **What**: Before coding, the agent asks the user structured questions: desired behavior, users, inputs, outputs, edge cases, failure modes, acceptance criteria. Grill-with-docs also checks living docs.
- **Why it works**: Reduces wrong-implementation rework by clarifying requirements upfront.
- **Weaknesses**: Relies on the user being available and patiently answering; no codebase-first investigation (the prompt says "investigate first, ask only what the codebase can't answer" — but the skill doesn't enforce this procedurally).
- **APEX copy**: The grilling question set as a /grill command. The "investigate first" rule — APEX must enforce it programmatically (run search/grep before asking).
- **APEX improve**: Auto-investigate before asking; learn the user's preferences (what questions they always answer the same way); produce a spec document from the answers.

### Domain-modeling / Codebase-design
- **What**: Build a shared domain language before coding; maintain CONTEXT.md + ADRs. Deep modules (small interfaces, deep behavior).
- **APEX copy**: CONTEXT.md + ADR discipline; the "shared domain language" concept.
- **APEX improve**: auto-generate from codebase analysis (LSP symbols, imports); auto-detect domain drift.

### TDD
- **What**: Red-green-refactor workflow. Write failing test first, then minimal code, then refactor.
- **APEX copy**: TDD mode (prefer tests before code; enforce for behavior changes).
- **APEX improve**: auto-detect whether TDD fits the task (greenfield feature → yes; quick fix → no); dependency-aware test selection.

### Diagnosing-bugs
- **What**: Always diagnose before modifying. Reproduce → observe → hypothesize → instrument → test hypothesis → locate root cause → minimal fix → verify → regression test.
- **Why it works**: The universal debug cycle, encoded as procedure.
- **APEX copy**: the debug loop verbatim; the "never randomly modify code until tests pass" rule.
- **APEX improve**: integrate with DAP debugger (APEX §5); instrument non-invasively (print/logger with auto-cleanup).

### Handoff
- **What**: Compact the conversation into a handoff: objective, current state, completed work, changed files, decisions, failed approaches, open questions, next action.
- **APEX copy**: handoff format verbatim; auto-generate at session end (APEX §16).
- **APEX improve**: structured JSON schema for machine consumption; auto-write to `.apex/` dir.

### To-spec / To-tickets
- **What**: Turn conversation into a formal spec or ticket set.
- **APEX copy**: spec format (goal, non-goals, requirements, constraints, architecture, data model, interfaces, edge cases, tests, acceptance criteria).
- **APEX improve**: make specs executable (another agent can implement from the spec without talking to the user).

### Code-review
- **What**: Review changes since a fixed point. Structured: correctness, security, architecture, tests.
- **APEX copy**: review checklist; commit/branch anchoring.
- **APEX improve**: independent reviewer agent (APEX §12); severity classification (INFO/WARNING/BLOCKER).

## 5. How the Pack Works at Runtime

- Installation: `.claude-plugin/` + `AGENTS.md` + `CLAUDE.md` integration. The `scripts/` dir has installation helpers.
- Loading: Claude Code's built-in skill system reads skills/ directory. The CLAUDE.md references the skills directory.
- The pack has NO runtime — skills are prompt-injection patterns. The harness reads the markdown and loads the procedure into the model's context.
- **Strength**: simple, portable, model-agnostic.
- **Weakness**: no versioning, no dependency resolution, no testing, no failure handling, no verification gates. The model must "follow instructions" — no enforcement.

## 6. Verdict for APEX

**Copy conceptually (top 5)**
1. SKILL.md format (frontmatter + trigger + procedure) — APEX's skill system (§17) should use this exact format.
2. Engineering discipline procedure content: grilling, diagnosing-bugs, domain-modeling, ADRs, TDD, handoff, code-review, spec-to-tickets — embed these as built-in/optional skills.
3. Trigger discipline: "When to Use" sections — APEX's skill router should match these.
4. Handoff format (objective/state/completed/decisions/open-questions/next-action).
5. Grill questions set — APEX's /grill command.

**Weaknesses**
- No verification, no failure handling, no versioning, no dependency resolution.
- No harness — assumes the agent's prompt system handles everything.
- Grill skills don't enforce "investigate first before asking" programmatically.
- Skills don't define their own output schemas.
- No test or eval infrastructure.

**Improve**: add verification steps, failure handling, test data, versioned frontmatter, output schemas, dependency fields.

**Reject**: the assumption of Claude Code as the only host; the lack of structured output. APEX must reimplement the procedures as native skills with versioning, verification, and testing.

**Reuse**: embed the SKILL.md content as the PROCEDURE portion of APEX skills (trigger + purpose + procedure). Do NOT use the skill files directly since they reference Claude Code-specific paths and commands. Reimplement the philosophy, not the files.