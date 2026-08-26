# ADR-0001: Rust Core vs Fork vs Reuse

Status: accepted
Date: 2026-08-25
Supersedes: none (this ADR records the basis decision reached after Phase 0 research)

## Context
APEX targets: cold start < 100 ms, idle RAM < 50 MB, no Python/Node dependency for
basic operation, native LSP/DAP/search/edit engines. Phase 0 research (see
`docs/research/architecture-comparison.md`) evaluated forking or reusing each
reference project as the core.

Research findings:
- **jcode**: Rust, closest performance profile, but bespoke daemon socket protocol,
  giant modules (4K-line files), hardcoded provider assumptions, no LSP/DAP, and
  its published RAM/TTFB numbers lack in-repo reproduction harnesses.
- **oh-my-pi**: Rust natives but TypeScript/Bun core (Node floor on RAM/startup),
  Bazel+nix build, 3.1M LOC monorepo.
- **hermes-agent**: Python, 2.8M LOC, 30K-line gateway modules. Best philosophy,
  wrong language.
- **ai-memory**: Rust memory server, excellent memory model, but a companion
  process — not an agent harness.
- **mattpocock/skills**, **i-have-adhd**: content packs, not harnesses.

## Decision
**Clean Rust implementation.** No fork. APEX builds a fresh Rust core adopting the
concepts (not the code) of all six projects per the lineage table in
`architecture-comparison.md`. Optional: ai-memory as an external companion for
heavy cross-harness memory; never required for basic operation.

## Alternatives considered
- Fork jcode — rejected: protocol/module restructuring would cost more than a clean core.
- Fork oh-my-pi — rejected: TS core + build weight violate the performance mandate.
- Fork hermes — rejected: Python runtime violates the performance mandate.
- Build on ai-memory as core — rejected: it is a memory server, not an agent.

## Tradeoffs
- Clean implementation costs more upfront (no existing agent loop to inherit).
- Zero inherited technical debt; full ownership of architecture and benchmarks.

## Consequences
- All reference code is MIT; concepts are reimplemented, no code copied without attribution.
- Benchmark suite is built in parallel with the core (measure from day one).
- ADRs 0002-0010 assume this decision.