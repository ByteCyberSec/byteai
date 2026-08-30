# VqDs46A8pqE — "Claude Code is Amazing... Until It DELETES Production"

## CORE_THESIS
Agents can permanently destroy production assets with one hallucinated or incorrectly prompted command — it only takes 1 in 100,000 errors. The solution is **Claude Code Damage Control**: a reusable skill that installs layered Claude Code hooks to prevent catastrophic, irreversible commands across all your codebases. The framing is about trust: you can build trust, defer it, or make it unnecessary (if agents *can't* run destructive commands, you don't need to trust them). The video narrates a near-miss where the author almost ran a catastrophic production command, motivating the standardized system.

## KEY_TECHNIQUES — three protection layers
1. **Deterministic pre-tool-use hooks** — a `patterns.yaml` file drives three scripts (bash tool, edit, write):
   - **Blocked commands**: regex patterns the agent can never run.
   - **Ask patterns** (`ask: true`): the hook intercepts, asks the user for permission before running (e.g., SQL operations).
   - **Path protection levels**: `zero_access` (cannot read/write/execute — e.g., .ssh), `read_only` (can read, cannot write/edit), `no_delete` (cannot delete — e.g., hook files, .bashrc).
2. **Prompt pre-tool-use hook (non-deterministic)** — a lightweight prompt catches *unknown* dangerous commands before they run; intended as a "last-ditch effort" — once a new dangerous command is caught, encode it into the deterministic layer. Caveat: slower (runs a prompt on every bash command).
3. **Global (user-level) hooks** — apply to every codebase on the device; merged into settings. Hierarchy: user → project → local → enterprise.

## TOOLS_AND_RECOMMENDATIONS
Claude Code Damage Control skill (public repo): patterns.yaml, bash/edit/write protection scripts, cookbook prompt (9-step agentic workflow), `/install` interactive command (ask-user-question tool: project/global/personal level, Python or TypeScript, merge workflow if settings exist). Uses astral uv or bun. Recommends installing on every production codebase. Also recommends sandboxes as a complementary trust-deferral strategy.

## APPLICABLE_PRINCIPLES
"You don't require trust if your agents can't run destructive commands" — safety is easier than reliability. The cost of a prompt hook on every bash command is less than the cost of deleting a production asset. Once you find a dangerous command you didn't know existed, encode it deterministically. A single bad command (1 in 100,000) can destroy months of work. Skills are the right packaging for reusable security resources.