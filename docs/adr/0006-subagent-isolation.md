# ADR-0006: Subagent Isolation

Status: accepted
Date: 2026-08-25

## Context
Parallel agents must not silently overwrite each other's changes. Isolated worktrees (Hermes' `-w` mode, oh-my-pi's `hindsight` worktree banks) and conflict detection are required.

## Decision
Each subagent gets an isolated git worktree (or equivalent overlay when git is not available). The coordinator tracks: files_read, files_modified, symbols_modified, branches, commits. Worktree roots are `.byteai/worktrees/<agent_id>/`. If two agents modify overlapping files, the coordinator flags the conflict and re-runs the affected tests.

Subagents return structured typed results: status, summary, findings, files_read, files_modified, tests_run, tests_passed, risks, recommendations, confidence (0-1). The parent can query any subagent's result fields directly.

## Alternatives
- All agents share one workspace: rejected — silent overwrites cause data loss.
- Branch-based isolation without worktrees: rejected — slower branch switching, harder to merge.

## Tradeoffs
- Worktrees add git overhead; avoids silent corruption.
- Structured results enable programmatic review.
- Conflict detection requires tracking file/symbol access.

## Consequences
- Subagent coordinator in `byteai-subagent/` crate.
- Typed result schemas in `byteai-protocol/`.
- Worktree management reusable for non-git projects (overlay symlinks).