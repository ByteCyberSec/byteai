# ADR-0008: Skills

Status: accepted
Date: 2026-08-25

## Context
Skills are the primary mechanism for learning from experience (procedural memory).
The strongest reference format is the SKILL.md pattern from hermes-agent and
mattpocock/skills. Skills must be portable, versioned, and testable.

## Decision
Skills are Markdown files with YAML frontmatter: name, description (first 57 chars
truncated for the index), trigger, purpose, inputs, procedure, verification,
failure handling, examples. Stored under `skills/<category>/<name>/SKILL.md`.
Lifecycle: experience → candidate lesson → reuse → validation → promotion to skill.
Track uses, successes, failures, last_updated, confidence. Skills that repeatedly
fail are revised or demoted.

Bundled skills: engineering discipline content from matt-pocock/skills
reimplemented (grilling, diagnosing-bugs, TDD, domain-modeling, ADR, handoff,
code-review, to-spec, to-tickets) — not copied verbatim.

## Alternatives
- Prompt injection of procedures at runtime (raw markdown packs): rejected — no
  versioning, verification, or failure handling.
- Code-based skills: rejected — not portable, not user-editable.

## Tradeoffs
- Markdown is not executable; procedures need the harness to interpret them.
- Verification steps in skills are advisory unless enforced by the harness.

## Consequences
- Skill loader + lifecycle in `byteai-skills/` crate.
- Bundled skills shipped with the binary; user skills in `.byteai/skills/`.
- `/skills`, `/learn`, `/forget` commands.