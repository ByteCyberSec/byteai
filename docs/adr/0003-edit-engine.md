# ADR-0003: Edit Engine

Status: accepted
Date: 2026-08-25

## Context
Edit success rate is the single most important reliability metric. Models write edit payloads in various formats (exact replacement, contextual, loose "sloppy", whole-file). A single-strategy editor fails on many real tasks.

## Decision
Multi-strategy edit engine with fallback: exact match → contextual match → sloppy-grammar (inspired by oh-my-pi's Lark grammar) → whole-file generation. After every edit: validate patch applied, syntax remains valid, LSP diagnostics do not introduce new critical errors, formatting is preserved. On failure, switch strategy immediately. Never repeat the same failed method three times.

## Alternatives
- Single strategy (exact only): rejected — fails on whitespace-ambiguous/format-changed files.
- LSP-only workspace edits: rejected — requires LSP server to be running and supports limited operations.

## Tradeoffs
- More complex edit engine; higher success rate.
- Validation adds latency per edit; prevents compounding errors.

## Consequences
- Edit engine lives in `byteai-edit/` crate.
- Grammar for sloppy format defined in a `.lark` file (or equivalent parser).
- Edit validation calls LSP diagnostics and typechecker optionally.