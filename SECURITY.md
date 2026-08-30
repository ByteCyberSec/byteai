# Security Policy

## Reporting a Vulnerability

ByteAi is a local coding agent. We take security seriously. If you find a
security vulnerability, please report it privately so it can be fixed before
public disclosure.

**Do NOT open a public issue for security vulnerabilities.**

### How to report

Email the maintainers, or open a private advisory on GitHub:
https://github.com/ByteCyberSec/byteai/security/advisories/new

Please include:

- A description of the vulnerability
- Steps to reproduce
- Affected versions
- Any suggested fix

## Scope

ByteAi executes shell commands, reads/writes files, and calls LLM APIs on the
user's machine. It runs with the privileges of the invoking user. Treat any
agent prompt or tool input as untrusted. If you find a way to make ByteAi:

- Exfiltrate secrets or config
- Write outside its working directory via a tool bug
- Execute commands via prompt injection beyond the configured model
- Crash or corrupt state

…please report it.

## Supported Versions

| Version | Supported |
|---------|-----------|
| main    | ✅        |

## Response

We aim to acknowledge reports within 48 hours and issue fixes promptly.