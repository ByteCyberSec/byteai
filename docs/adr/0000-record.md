# ADR-0000: Record — ADR template and status legend

Status: accepted (process record)
Date: 2026-08-25

## Purpose

This file is the index and template for Architecture Decision Records (ADRs)
for ByteAi. Every significant architectural choice is recorded
as an ADR with: Context / Decision / Alternatives / Tradeoffs / Consequences.
ADRs are immutable once accepted; corrections are new ADRs that supersede.

## Status legend

- `proposed` — under discussion, not decided
- `accepted` — decided, in effect
- `superseded by ADR-NNNN` — replaced by a later decision
- `rejected` — considered and deliberately not adopted (recorded so the
  consideration is not repeated)

## ADR index

| ADR | Title | Status |
|---|---|---|
| 0001 | Rust core vs fork vs reuse | accepted (clean Rust implementation) |
| 0002 | Tool protocol | accepted |
| 0003 | Edit engine | accepted |
| 0004 | Context management | accepted |
| 0005 | Memory design | accepted |
| 0006 | Subagent isolation | accepted |
| 0007 | Provider routing | accepted |
| 0008 | Skills | accepted |
| 0009 | Security model | accepted |
| 0010 | License and attribution strategy | accepted |

## Template

```markdown
# ADR-NNNN: Title

Status: proposed | accepted | superseded by ADR-XXXX | rejected
Date: YYYY-MM-DD

## Context
Why this decision is needed; constraints; relevant research findings.

## Decision
What we decided, in one or two paragraphs. The decision is the WHAT and the
WHY — not the implementation detail.

## Alternatives considered
What else was evaluated and why it was rejected (or kept as fallback).

## Tradeoffs
Costs and benefits; what we give up.

## Consequences
What changes as a result; what future ADRs this unlocks or constrains.
```
