# -WBHNFAB0OE — "AGENT THREADS. How to SHIP like Boris Cherny. Ralph Wiggum in Claude Code."

## CORE_THESIS
Agentic engineering is a new skill; you need a framework to measure progress ("if you don't measure it, you can't improve it" — even Karpathy feels left behind). Thread-based engineering: a **thread is a unit of engineering work over time driven by you + your agents**. You show up at the two mandatory nodes — the prompt/plan (start) and the review/validation (end); the middle is your agent's tool-call chain. **Tool calls ≈ impact** (assuming useful prompting), and improving = increasing total tool calls your agents make on your behalf. Boris Cherny (Claude Code creator) ships by defaulting to 5 parallel Claude Codes in numbered terminals + 5–10 more in the Claude Code web interface via `@`, with `--dangerously-skip-permissions` off and specific permissions instead.

## KEY_TECHNIQUES — six thread types
- **Base thread**: single prompt → tool calls → review.
- **P-thread (parallel)**: multiple concurrent threads (terminals, worktrees, sandboxes); `fork terminal` skill + `pthread` alias.
- **C-thread (chained)**: intentional phase-chunking for context limits or high-pressure production work; re-enter loop via ask-user-question, system notifications, TTS hook.
- **F-thread (fusion)**: same prompt to N agents (3 Claude + 3 Gemini + 3 Codex via `mros` + sandboxes), best-of-N / cherry-pick / merge; more shots = more confidence.
- **B-thread (big)**: meta-structure — prompts firing off prompts (subagents, orchestrator spawning plan→scout→build→review→staging); black box from your seat.
- **L-thread (long)**: high-autonomy, hours-long (Boris: 1 day 2 hours), hundreds/thousands of tool calls; Ralph Wiggum = agents + code loop.
- **Z-thread (hidden 7th)**: zero-touch — no review node, maximum trust.

## TOOLS_AND_RECOMMENDATIONS
Claude Code (terminal + web), Gemini, Codex, mros, agent sandboxes, fork-terminal skill, pthread/parallelize skills, text-to-speech hook, stop hook (decision code: check progress file, run validation, reloop or complete). Recommendation: build your own net-new agentic layer around production codebases; give agents a way to verify their own work (validation/closed loop).

## APPLICABLE_PRINCIPLES
Four ways to improve: **run more threads, longer threads, thicker threads, fewer human-in-the-loop checkpoints**. Everything reduces to the core four (context, model, prompt, tools). Scale compute to scale impact; don't babysit a single agent (scale down if you have to). Reduce review nodes by giving agents self-validation tools, reserving human checkpoints for genuinely critical work.
