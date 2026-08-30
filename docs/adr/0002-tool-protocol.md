# ADR-0002: Tool Protocol

Status: accepted
Date: 2026-08-25

## Context
Every tool call in the agent loop needs a well-defined contract: schema, result shape, failure taxonomy, timeout, permissions. A typed protocol reduces model errors, enables accurate failure classification, and supports tool-role permissions.

## Decision
Tools are defined as native Rust implementations with typed JSON schemas (serde + schemars or similar). Each tool declares: name, description, input schema, output schema, timeout, permissions, run cost category. The dispatcher routes tool calls to the implementation, enforces timeouts, captures output, classifies failures, and returns structured results.

MCP is supported for external extensibility only — core high-frequency tools are never MCP round-trips.

## Alternatives
- All tools as MCP servers: rejected — adds latency and complexity for core tools.
- All tools as free-form shell commands: rejected — no schema, no failure classification, no safety.

## Tradeoffs
- More upfront work per tool; higher reliability and correctness.
- MCP is second-class by design, keeping the hot path fast.

## Consequences
- Tool schemas live in `byteai-tools/` crate.
- Failure taxonomy is defined as an enum in `byteai-protocol/`.
- MCP adapter bridges external tools to the native dispatcher.