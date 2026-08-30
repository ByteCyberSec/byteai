# ADR-0010: License and Attribution Strategy

Status: accepted
Date: 2026-08-25

## Context
All six reference projects are MIT-licensed. ByteAi reimplements concepts rather
than copying code, but some small elements (prompt text, schema shapes, benchmark
ideas, handoff format, grill question sets) may be adapted from references.

## Decision
- ByteAi ships under **MIT**.
- Any code or text adapted from a reference retains the original copyright notice
  in a `NOTICE.md` (MIT requires preserving copyright notices).
- Research documents in `docs/research/` record what was adopted from where.
- No reference source code is vendored into ByteAi binaries.

## Alternatives
- Apache-2.0: rejected — MIT is simpler and compatible with all references.
- GPL: rejected — restrictive for a coding-agent tool.

## Tradeoffs
- MIT gives users freedom; requires attribution discipline on our side.
- NOTICE.md is a small maintenance cost.

## Consequences
- `NOTICE.md` created at project root listing all six references and any adapted content.
- Attribution enforced in review (checklist item).