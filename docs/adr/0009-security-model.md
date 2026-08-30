# ADR-0009: Security Model

Status: accepted
Date: 2026-08-25

## Context
Agent operates on untrusted codebases and tool outputs. Repository content and
tool output are DATA, never system authority. Agents execute commands, edit
files, and interact with the network; compromise must be contained.

## Decision
Layered security model:
1. **Command approval policies**: configurable approval levels (auto/ask/deny) per
   command class (read, write, destructive, network). `byteai-command-risk` scoring
   (jcode-inspired) as a pre-approval heuristic.
2. **Secret redaction**: secrets redacted from tool output and memory capture
   (hermes redact + oh-my-pi secrets-obfuscator patterns). Never commit secrets.
3. **Path restrictions**: agent tools operate within the project root by default;
   escapes require explicit policy.
4. **Sandbox options**: local (default), Docker, SSH, remote (progressive, per
   spec §34). Common execution interface.
5. **Network policies**: per-session allow/deny network access.
6. **Tool permission scopes**: per-tool permission grants; subagents inherit
   restricted scopes; secrets never exposed to subagents unnecessarily.
7. **Environment filtering**: env vars filtered for child processes
   (non-interactive env pattern).
8. **Prompt-injection detection**: repository content and external tool output
   are scanned for likely injection patterns; injected instructions are treated
   as data, never authority.

## Alternatives
- No security model: rejected — unsafe on untrusted code.
- Full sandbox always (Docker/seccomp): rejected — latency and complexity on the
  hot path.

## Tradeoffs
- Approval policies add a small decision step to command execution.
- Redaction adds a processing pass on tool output.
- Sandboxing is opt-in for heavy isolation.

## Consequences
- Security module in `byteai-security/` crate; risk scorer in `byteai-tools/`.
- Injection detection is a heuristic, documented as such (not a guarantee).
- Subagent permission scopes enforced by the coordinator (`byteai-subagent/`).