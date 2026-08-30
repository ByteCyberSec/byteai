//! Dan Methodology tool — embedded knowledge base distilled from all 51
//! @indydevdan YouTube videos (agentic engineering, harness engineering,
//! skills systems, multi-agent orchestration, model stacking, sandboxes,
//! security, observability, context engineering, planning, local models).
//!
//! The agent can query it by topic during any turn; the returned guidance is
//! immediately actionable inside ByteAi (skills, spawn, sandbox, plan,
//! council, route, moa, memsearch, github tools).

use std::time::Instant;

use byteai_types::{ToolDef, ToolOutcome};
use serde_json::{json, Value};

use crate::{BoxFuture, Tool, ok_outcome};

/// Topic keys accepted by the tool.
const TOPICS: &[(&str, &str)] = &[
    ("overview", "The full distilled methodology at a glance."),
    ("harness_engineering", "The systems you place around agents: skills, tools, sandboxes, prompts, context."),
    ("skills_system", "Skills as the unit of agent capability; the library meta-skill; distributing skills/agents/prompts."),
    ("agent_sandboxes", "E2B and isolated execution environments for safe agent scaling."),
    ("multi_agent", "Agent teams: Pi-to-Pi, CMUX, tmux, CEO agents, subagents, orchestrator patterns."),
    ("model_selection", "Fusing/stacking models instead of picking one; when models matter and when they don't."),
    ("security", "Agentic security: kill the BASH tool, sandbox every command, protect production."),
    ("observability", "Making agent work reviewable: HTML specs, review agents, verification gates."),
    ("context_engineering", "Managing context windows, 1M context, compaction, elite context discipline."),
    ("planning", "/PLAN skills, plan-first agentic coding, task systems."),
    ("local_models", "MLX / GGUF local stacks, Gemma, killing model-provider lock-in."),
    ("agent_threads", "The continuous-improvement mental framework for shipping with agents."),
    ("software_factory", "The complete repeatable system that turns prompts into shipped software."),
    ("prompt_engineering", "Prompt engineering is NOT dead — agentic prompt engineering for you, your team, and your agents."),
    ("agents_learning", "Agent Experts: agents that actually learn from their own history."),
];

const OVERVIEW: &str = r#"INDUSTRIAL-STRENGTH AGENTIC ENGINEERING — the distilled methodology of @indydevdan (Andy Devdan), from all 51 videos on his channel.

Definition: Agentic engineering = engineering WITH intelligence that can operate on your behalf. It is NOT vibe coding (prompting until something works) and NOT just using Claude Code. It is the craft of building the systems around agents so they ship production software while you sleep.

Core creed (repeated across every video):
1. "What matters is the systems you place around your agents." Models matter less and less — 80-90% of daily work is won by harness, not hardware. Models matter only for bleeding-edge work.
2. "If you want to scale your impact, you must scale your compute." Give more work to your agents; show up where it matters most: PLANNING and REVIEWING.
3. "Living software that works for us while we sleep." The mission: agents as a workforce, not a chatbot.
4. Continuously improve: the ceiling keeps moving. Operate in THREADS, not one-off prompts.
5. Stay skeptical of model-provider lock-in: local-first (MLX/GGUF) where possible; fuse models (stack them) instead of picking a single winner.
6. The CORE FOUR — every agent outcome is composed from context, model, prompt, tools. Master those four; the rest is detail. "If you can bypass everything by understanding the core four, you control your stack."
7. TRUST is the 2026 meta-metric: trust = speed = iteration = impact. The longer agents run before a mistake you must correct, the more you scale. Build trust via sandboxes (make it unnecessary), hooks (make it deterministic), and review (make it visible).

The five big ideas he re-centers on:
1. Agentic engineering is the #1 opportunity for senior engineers; by end of 2026 it is the default. The early window is closing.
2. Harness engineering — the scaffolding around agents (skills, tools, sandboxes, prompts, context) is where leverage lives.
3. Skills are the unit of agent capability — build, version, and distribute them like libraries.
4. One agent is NOT enough — multi-agent teams (orchestrator + specialists + reviewers) ship what a single agent cannot.
5. Safety and reviewability first — sandbox everything, delete the raw BASH tool, make agent work reviewable (HTML specs, review agents)."#;

const HARNESS_ENGINEERING: &str = r#"HARNESS ENGINEERING (the core discipline)

Thesis: The agent is the CPU; the HARNESS is the motherboard. Two engineers with the same model produce wildly different results because of what they place AROUND the agent. This is the single most repeated idea on the channel.

Components of a harness (build all of them):
1. Skills — the agent's operational memory. A skill = directory with SKILL.md (name, description, trigger, body). Load on demand, not everything at once. Version them like code.
2. Tools — narrowly-scoped, well-described functions the agent calls. Fewer, sharper tools beat many sloppy ones. (He literally advocates DELETING the raw BASH tool and replacing it with safe, sandboxed, task-specific tools.)
3. Prompts / AGENTS.md — one prompt every agentic codebase should have: project context, conventions, what the agent may/may not touch, definition of done. Engineers write it once; every agent and human teammate reads it.
4. Sandboxes — isolated execution (E2B, containers, ephemeral VMs) so agents can run code, browsers, and commands without touching production or the host.
5. Context management — structured context engineering (see context_engineering): keep the window clean, compact, and loaded with what matters; 1M-context models change what you can keep resident.
6. Observability — every agent action must be reviewable: HTML specs, diffs, review agents, verification gates before merge.
7. Review loop — a second agent (or a "reviews like YOU" agent with your standards baked into its prompt) critiques the first's work before it ships.

Rules of thumb:
- "Prompt engineering is NOT dead" — it moved from the chat box into the harness (skills, system prompts, tool descriptions).
- Small, fast models for routine work; big models for planning/architecture/review. Route by task, not by habit.
- Every harness needs a /PLAN step: decompose before executing. Plan skill = the single highest-ROI skill.
- Measure your harness: iterations, tool calls, tokens, review catches. Improve the system, not the individual prompt."#;

const SKILLS_SYSTEM: &str = r#"SKILLS SYSTEM (the unit of agent capability)

A skill is a directory with a SKILL.md: YAML frontmatter (name, description, trigger conditions) + markdown body (instructions, steps, pitfalls, verification). The agent loads it only when the trigger matches, so context stays lean.

How to build skills (his recipe):
1. Capture after a hard task — turn the working solution into a reusable skill immediately.
2. Frontmatter description = the TRIGGER contract. Write "Use when <situation>." so the agent knows exactly when to fire it.
3. Body = numbered steps, exact commands, pitfalls you hit, and a verification section. No fluff.
4. Iterate: a skill you never use is debt; a skill that fails in the field gets patched immediately.
5. Version and distribute them like libraries.

The LIBRARY META-SKILL (his distribution pattern):
- Keep a master library of private skills/agents/prompts (his "Library" = a git repo of skills).
- A meta-skill "install from library" copies the right skills/agents/prompts into a project on demand — private distribution without a public marketplace.
- Teams share one library: skills become the shared language of the engineering org.
- This is how a senior engineer's expertise becomes the team's default behavior: bake it into skills, not into hallway conversations.

Skills as the moat:
- "I finally CRACKED Claude Agent Skills" — skills are the difference between a generic assistant and a domain-expert agent.
- When models commoditize (he predicts Sonnet/Opus-level local models by year-end), skills + harness are the remaining differentiation.

Practical: every AGENTIC codebase should ship with an AGENTS.md + a skills/ directory; agents read the context, then load skills on demand. ByteAi already implements this exact model via its skills tool and <data_dir>/skills/."#;

const AGENT_SANDBOXES: &str = r#"AGENT SANDBOXES (the space to place your agents)

Why: agents need to RUN things — code, tests, browsers, installs — and the host machine is the wrong place for that. Sandboxes give agents a space to act with three wins: SAFETY (can't delete production), REPRODUCIBILITY (fresh environment every time), and SCALE (spin up N agents in parallel).

What he uses / recommends:
- E2B (e2b.dev) — cloud sandboxes with a simple SDK; the "space to place your Claude agents." Instant ephemeral VMs, filesystem + process APIs, works with any agent harness.
- Local containers / ephemeral VMs on your own hardware for private work.
- Agent sandbox CODEBASE pattern: a repo that lets you "quickly spin up agents against specific codebases" — each agent gets its own copy/workspace, spends time understanding the code, then acts. Layered, not simple.

The mental model:
- Your agents are a workforce; sandboxes are their desks. Give each agent a clean desk, its own copy of the repo, and the tools it needs.
- The software factory NEEDS agent sandboxes to SCALE: when you go from 1 agent to 10, the bottleneck is no longer the model — it's shared mutable state. Sandboxes kill that bottleneck.

Safety rules (non-negotiable):
1. Agents never execute against production directly — sandbox first, promote after review.
2. Destructive commands (rm, DROP, git push --force) live behind explicit gates.
3. Secrets never enter the sandbox unless scoped; sandboxes are disposable.
4. Browsers/Playwright run inside the sandbox, not on the host.

ByteAi mapping: the sandbox tool + spawn tool + review tool are exactly this pattern — run delegated agents in isolated worktrees/sandboxes, review before merge."#;

const MULTI_AGENT: &str = r#"MULTI-AGENT ORCHESTRATION (one agent is NOT enough)

Thesis: single-agent flows top out. The next level of agentic engineering is TEAMS: orchestrator + specialists + reviewers, each with its own harness role, working in parallel, communicating through files and structured channels.

Patterns he demonstrates:
1. Orchestrator → workers: a lead agent decomposes the mission, spawns specialist subagents (each in its own sandbox/worktree), collects results, integrates.
2. Reviewer agents: "an agent that REVIEWS like YOU" — bake your code-review standards into a reviewer agent's prompt; it catches what the writer misses. GPT-5.5 verified Opus 4.7-style: a separate model/agent reviews the work.
3. Pi-to-Pi / agent-to-agent: two agents talk over a channel (files, tmux panes, CMUX) — one plans, one executes; or two agents debate/verify each other's work.
4. CEO agent pattern: a single coordinating agent with 1M context holds the whole project state (docs, plans, tickets) and delegates execution to specialist agents with fresh, small contexts.
5. CMUX / tmux multiplexing: run multiple agents side-by-side in terminal panes; watch them work; intervene per-pane.
6. Swarm / council: for decisions, multiple models deliberate (model council) instead of one model guessing.

Rules:
- Each agent gets ONE job and a tight context. Deep specialists beat generalists.
- Communication = files + structured messages, not chat soup. Agents write plans/artifacts the next agent reads.
- Always include a reviewer role in any team of 3+ agents.
- Subagents inherit the mission but get independent budgets (iterations, wall-clock) so one runaway can't kill the team.

ByteAi mapping: spawn tool (parallel subagents with independent budgets), crew tool, council tool (multi-model deliberation), review tool — this is the multi-agent team pattern built in."#;

const MODEL_SELECTION: &str = r#"MODEL SELECTION & STACKING (don't pick one — FUSE them)

Thesis: "STOP picking GPT-5.6 Sol OR Claude Fable 5 — FUSE THEM." The winning move is not choosing a single model but stacking models by strength. Model-agnostic harness + per-task routing = better results than any one flagship.

His model philosophy:
1. Models matter less every month. 80-90% of daily engineering work is won by the harness (skills, tools, context). Reserve flagship models for planning, architecture, and review.
2. FUSE / STACK: run the same task through 2+ strong models and merge, or route subtasks to the best model for each (a coder model, a reviewer model, a cheap fast model for routine edits).
3. Model councils: multiple models vote/deliberate on high-stakes decisions (his council/govern tools pattern). Diversity beats a single oracle.
4. Watch for "model class" jumps: each new class (Mythos/Fable-class reasoning, 1M context) changes what harness you should build — rebuild your /PLAN skill when the model class changes.
5. Don't be loyal to a provider: benchmark, vibe-check, and swap. "Claude Fable 5 BANNED: the first model agentic engineers don't need" — some launches are hype; skip them.
6. Local models are the endgame for cost/privacy/speed: MLX on Apple Silicon (Gemma 4, etc.) runs real agent work today; by year-end expect Sonnet/Opus-class local.

Routing rules of thumb:
- Tiny/quick edits, classification, extraction → small fast model.
- Coding loops, tool use, multi-step tasks → mid-size workhorse (Sonnet-class).
- Planning, architecture, conflict resolution, final review → flagship (Opus-class).
- Never put your whole pipeline on one model: a single rate-limit/outage/deprecation shouldn't stop the factory.

ByteAi mapping: route tool (per-task model routing), moa tool (mixture-of-agents fusion), council tool — the fuse-them pattern, native."#;

const SECURITY: &str = r#"AGENTIC SECURITY (delete the BASH tool)

Thesis: raw shell access is how agents delete production. "Claude Code is Amazing... Until It DELETES Production." The fix is not trust — it's architecture.

His security rules:
1. DELETE the raw BASH tool from the agent's toolset. Replace it with narrowly-scoped tools: safe file ops, git ops with guards, package ops in sandbox, DB ops with dry-run. An agent with 20 sharp tools is safer and MORE effective than one with bash.
2. Sandbox everything that executes (see agent_sandboxes). Commands run in an ephemeral environment; nothing touches the host or prod.
3. Gate destructive actions: rm, DROP TABLE, force-push, deploy → explicit approval gate or a review agent.
4. Secrets hygiene: API keys never enter prompts or logs; scoped credentials per sandbox; nothing stored in the repo.
5. Subscription/usage safety: know your rate limits; parallel agent farms can burn a subscription fast ("MAXIMIZE your Claude Code subscription WITHOUT getting banned" — batch, cache, route cheap work to cheap models).
6. Data privacy: question providers' data policies ("Is Anthropic STEALING Your Data?") — prefer local models for sensitive code; know what your agent tool sends where.

Pattern: every agent action ends in a reviewable artifact (diff, spec, output log). If a human can't see what the agent did, it didn't happen.

THE DAMAGE-CONTROL LAYER (his reusable "Claude Code Damage Control" skill — install it on every production codebase):
"An agent can permanently destroy production with one hallucinated command — it only takes 1 in 100,000 errors. You don't require trust if your agents CAN'T run destructive commands."
1. Deterministic pre-tool-use hooks driven by a patterns.yaml:
   - BLOCKED commands: regex patterns the agent can never run.
   - ASK patterns: hook intercepts and asks the user before running (e.g. SQL).
   - PATH PROTECTION levels: zero_access (can't read/write/execute — e.g. .ssh), read_only (can read, not write), no_delete (can't delete — e.g. hooks, .bashrc).
2. Prompt-based pre-tool-use hook (non-deterministic): a lightweight prompt catches UNKNOWN dangerous commands as a last-ditch; once caught, encode it deterministically. Slower (a prompt per bash call) — so use sparingly.
3. Global user-level hooks applied to every codebase; hierarchy user → project → local → enterprise.
4. Self-validation hooks (the feature seniors miss): hooks can now live INSIDE skills/subagents/commands — a command carries its own validator script (post_tool_use) that parses the output, logs to its own file, and on failure returns "resolve this error in <path>" so the agent fixes its own work. Guarantee beats prompting: a hook ALWAYS runs.

ByteAi mapping: secrets tool, worktree tool (isolated branches), review tool, gates tool (policy gates), sandbox tool — the security posture is built in; the BASH-tool-kill is the model to follow (use the sharp tools instead of shell)."#;

const OBSERVABILITY: &str = r#"OBSERVABILITY (make agent work reviewable)

Thesis: you can only scale agents you can trust, and you can only trust what you can review. Every agent run should produce artifacts a senior engineer can audit in minutes.

His observability stack:
1. HTML SPECS: before/after agent work, generate an HTML spec of the intended change (with Gemini Flash / GPT Image-class models producing visual specs) so a human sees the design before code lands.
2. REVIEW AGENTS: a second agent whose prompt encodes YOUR review standards ("reviews like YOU") critiques diffs, catches regressions, and blocks bad merges. GPT-5.5 verified Opus 4.7: the reviewer is often a stronger model than the writer.
3. Diffs as the unit of truth: agents work in branches/worktrees; every change is a reviewable diff; nothing merges unreviewed.
4. Live status: while an agent works, show what it's doing (phase, active tool, iterations) — never a black box. (ByteAi's live-status line does exactly this.)
5. Structured output: tools return structured results (JSON), not wall-of-text; the agent and the human both parse them.
6. After-action: iterations, tool calls, tokens, review catches — log them and feed them back into the harness (this is how agents learn; see agents_learning).

Rule: "If you can't review it, the agent didn't do it." Review is a first-class role in every agent team, not an afterthought.

ByteAi mapping: live status line, review tool, verify tool, tool cards in the TUI — observability is native; deepen it by generating HTML specs before big changes and routing review through a stronger model."#;

const CONTEXT_ENGINEERING: &str = r#"CONTEXT ENGINEERING (elite discipline)

Thesis: context is the agent's working memory — and the #1 lever on output quality. "Elite Context Engineering with Claude Code" = knowing exactly what to put in the window, in what order, and what to keep OUT.

His practices:
1. AGENTS.md / project context: one canonical file per repo — goals, architecture, conventions, do/don't, definition of done. Loaded at session start; every agent and human reads the same truth.
2. Structured loading: pull context by need, not by default. Skills load on trigger; docs load by reference; the window stays lean.
3. Compaction: when the session gets long, COMPRESS — summarize the conversation into a compact state (his /compress pattern) instead of letting the window fill with noise. Compact before the model starts losing the thread.
4. Big-context models change the calculus: 1M-context models (Claude 1M, Pi CEO agents) let you keep entire repos + plans resident — but even then, structure beats dump. Put indexes, plans, and tickets in; leave logs and chatter out.
5. Order matters: put the mission + constraints first, then relevant context, then the ask. The agent reads top-down.
6. Fresh contexts for specialists: subagents get tight, task-specific contexts — a deep specialist with 5K tokens of perfect context beats a generalist with 100K of mush.
7. Memory layers: durable memory (notes, lessons, captured skills) outside the window, injected on relevance — 4-layer memory style.

Rule: every token in the window must earn its place. If it isn't shaping the output, it's noise.

ByteAi mapping: system prompt + AGENTS.md, memsearch tool (TF-IDF relevance injection), memory tool (notes), compress command — the 4-layer memory + relevance injection is the elite pattern built in."#;

const PLANNING: &str = r#"PLANNING (/PLAN — the highest-ROI skill)

Thesis: the #1 difference between top-2% engineers and everyone else is PLANNING. "TOP 2% Engineering: /PLAN 2026" — plan first, execute second, every time. His /PLAN skill is the most-rebuilt skill on the channel because it's the most valuable.

The /PLAN pattern:
1. STOP. Do not edit code on the first prompt. The agent first produces a PLAN: goals, constraints, file-by-file change list, risks, test strategy.
2. The human (or a reviewer agent) approves/edits the plan. Planning is where YOU show up.
3. Only then does execution begin — and execution is mostly mechanical once the plan is right.
4. Plan versioning: plans are artifacts (markdown in the repo), so the team sees the intent, and the next agent resumes from the plan instead of re-deriving it.
5. REBUILD the plan skill when the model class changes: a Mythos-class (reasoning-heavy) model plans differently than a fast model — your plan prompt must match the model's strengths ("Rebuilding my /Plan skill for Mythos-class models").

Task system (his anti-hype version):
- Break work into explicit tasks (a TASK SYSTEM file/board), each with: context, acceptance criteria, done-verification.
- Agents execute task-by-task, marking progress; the human reviews at checkpoints, not at the end.
- "Agent threads": a running stream of tasks threaded over days — you improve by giving agents MORE work each week, and reviewing better each week.

Rule: "One step is not enough. You need to be thinking about how to continuously improve what you can do with agents — planning and reviewing is where you add value."

ByteAi mapping: plan tool (plan artifacts), todo tool (task tracking), kanban tool (task board), improve tool — the /PLAN-first workflow is native; use it before every non-trivial change."#;

const LOCAL_MODELS: &str = r#"LOCAL MODELS (kill the model-provider lock-in)

Thesis: local models on your own hardware are private, cheap, fast, and increasingly GOOD. "My M5 Max, Gemma 4, MLX LOCAL stack. This KILLS model providers."

His stack:
1. Apple Silicon + MLX: use the dedicated MLX-quantized models (not GGUF) on Apple hardware — MLX is Apple's native format, faster and lower-memory than GGUF on the same chip.
2. GGUF vs MLX: GGUF (llama.cpp) is portable across vendors; MLX is the Apple-native winner for M-series. Match format to hardware.
3. Model picks: Gemma (Google) has been a standout local workhorse; watch Alibaba (Qwen), NVIDIA, and Apple's own models.
4. Local for the boring-but-constant work: embeddings, classification, extraction, small edits, private code analysis → local. Big reasoning → cloud flagship.
5. Privacy: sensitive code never leaves the machine. This is the answer to provider data-policy fears.
6. Cost: local = $0 marginal. A local stack can replace a large chunk of paid API traffic.
7. His prediction: by end of year, a Sonnet/Opus-4.0-level model runs on-device. The harness you build today for local models pays off then.

Benchmarking rule: benchmark AND vibe-check local models before trusting them — speed is meaningless if the output is wrong. When they're ready, you get the privacy/speed/cost benefits "the model providers don't want you to have."

ByteAi mapping: config supports custom base_url (point it at a local server like OmniRoute/ollama/MLX), route tool can send routine work to local models — the local-first stack is config, not code."#;

const AGENT_THREADS: &str = r#"AGENT THREADS (the improvement mental framework)

Thesis: how do you KNOW you're improving as an agentic engineer? Not by one prompt — by THREADS: continuous, compounding streams of agent work. Boris Cherny-style shipping: a thread is a long-running mission (a feature, a refactor, a project) carried by agents over days, with the human at plan and review.

The thread pattern:
1. A thread = mission + context + running plan + accumulated artifacts (diffs, specs, decisions). It lives in the repo (files), not in the chat.
2. Each day/week you extend the thread: agents pick up the next task, you review what they did, you tighten the plan.
3. Improvement metric: are you giving your agents MORE work each week, and are you reviewing BETTER? If yes, you're improving — regardless of the model.
4. Ralph Wiggum caution: without a thread, agents act like Ralph Wiggum — cheerful, confident, wrong. The thread's plan + review loop catches the wrongness.
5. Scale rule: "If you want to scale your impact, you must scale your compute" — threads are how compute compounds: the same repo, more agents, more parallel work, reviewed at the seams.

Why threads beat prompts:
- One prompt = one shot, forgotten tomorrow.
- A thread = compounding state: every session starts from the plan + artifacts, not from zero.
- Threads make agent work reviewable and auditable — a senior engineer can read the thread and know exactly what happened and why.

THE SEVEN THREAD TYPES (his taxonomy — run more, longer, thicker, fewer-checkpoints):
- BASE thread: single prompt → tool-call chain → review. The unit.
- P-thread (PARALLEL): many concurrent threads across terminals/worktrees/sandboxes; `fork terminal` + parallelize aliases.
- C-thread (CHAINED): intentional phase-chunking for context limits or high-pressure prod work; re-enter the loop via ask-user-question, system notifications, TTS hooks.
- F-thread (FUSION): same prompt to N agents (3 Claude + 3 Gemini + 3 Codex) in sandboxes, best-of-N / cherry-pick / merge — more shots = more confidence.
- B-thread (BIG): meta-structure — prompts firing prompts (orchestrator spawning plan→scout→build→review→staging); a black box from your seat.
- L-thread (LONG): high-autonomy, hours-long, hundreds/thousands of tool calls (Boris Cherny ships 1-day+ threads); the agent-loop is the work.
- Z-thread (ZERO-TOUCH, hidden 7th): no review node, maximum trust.

Improvement metrics: FOUR ways to get better — run MORE threads, LONGER threads, THICKER threads (more tool calls each), and FEWER human-in-the-loop checkpoints (give agents self-validation so you only review the critical nodes).

ByteAi mapping: plan tool + todo/kanban + worktree (branch per thread) + session resume — the thread pattern is native. Start every non-trivial mission as a thread: plan file + task board + branch."#;

const SOFTWARE_FACTORY: &str = r#"SOFTWARE FACTORY (prompts in, software out)

Thesis: a software factory is the repeatable SYSTEM that turns a prompt into shipped, reviewed, production software — reliably, at scale, with agents as the workforce. "My Super Simple Software Factory (For Agentic Engineers)" + "Your Software Factory NEEDS Agent Sandboxes to SCALE."

The factory pipeline:
1. INTAKE: mission/feature request → clarify into a spec (one prompt every agentic codebase should have: the repo context file).
2. PLAN: /PLAN skill produces the file-by-file plan; human approves.
3. EXECUTE: agents work in sandboxes/worktrees, task-by-task, with a task system tracking progress. One agent is never enough — spawn specialists.
4. REVIEW: review agents + human review the diffs; nothing merges unreviewed. Observability artifacts (HTML specs, diffs) make this fast.
5. SHIP: verified changes merge; tests pass; deployment is gated and observable.
6. LEARN: after-action metrics (iterations, catches, tokens) feed back into skills and prompts — the factory improves itself.

Cloudflare's model (he rated it S-tier tokenomics): tokenomics matter — the factory must be cost-aware; route cheap work to cheap models, batch, cache. A factory that burns tokens is a toy.

Rules for a working factory:
- Agents get: context (AGENTS.md), skills (library), sandboxes (safe execution), plans (direction), review (quality).
- Humans get: planning, reviewing, and the mission. Everything else is delegated.
- The factory is a system, not a script: it survives model changes, team changes, provider changes.

THE AGENTIC-LAYER GRADE MAP (from "The Codebase Singularity" — grade your layer, then improve it):
- Grade 1 (thinnest): AGENTS.md memory file + a /prime command (on-demand, tunable memory — read specific files when needed).
- Grade 2: specialized prompts + specs/ plan files + AI-docs directory + subagents (fetch-docs, test-writer) → specialization, parallelization, planning-before-implementation.
- Grade 3: custom tools — skills + MCP servers + scripts-as-tools (start/stop app, DB interaction). Pitfall: too many tools, token burn, overengineering — many engineers get stuck here. Remember you can bypass everything by understanding the core four.
- Grade 4: feedback loops — closed-loop prompts (request → validate → resolve), review/reproduce-bug/test prompts; agents review their own work; self-correcting agents.
- Grade 5+ (Class 3): an ORCHESTRATOR agent that kicks off arbitrary AI developer workflows end-to-end (plan → build → review → fix), multiple workflows concurrently. "The codebase singularity": you no longer work on the application — you work on the agents that run the application.

Onboarding-as-code (his "one prompt every agentic codebase should have"): a Justfile launchpad standardizes every workflow; a Setup hook runs install/maintenance (deterministic scripts + logging) wrapped with agentic prompts (/prime → /install interactive vs one-shot); setup.log is read back so the agent reports success/failure. "Living documents that execute" — when something changes you update the script AND the prompt, not stale docs. Agents + code beat either alone.

ByteAi mapping: everything the factory needs is native — plan, todo/kanban, spawn, sandbox, worktree, review, verify, skills, route, moa, council, github. Run ByteAi as a factory: intake → plan → parallel agents in worktrees → review → merge."#;

const PROMPT_ENGINEERING: &str = r#"PROMPT ENGINEERING (NOT dead — it moved into the harness)

Thesis: "FIXING Opus 5: PROOF that Prompt Engineering IS NOT DEAD." Prompt engineering didn't die — it moved from the chat box into the harness: skills, system prompts, tool descriptions, AGENTS.md. The same model, with a better prompt system, produces dramatically better work.

Agentic prompt engineering (for you, your team, AND your agents):
1. For YOU: write prompts as specifications — mission, constraints, context pointers, output contract, verification steps. Not paragraphs of pleading.
2. For YOUR TEAM: encode org knowledge into prompts/skills/AGENTS.md so every engineer gets senior-level context on day one.
3. For YOUR AGENTS: the best prompt is a SKILL — reusable, versioned, triggered by description, improved from field failures. Prompt once, apply forever.

Prompt structure he uses:
- Role + mission (one sentence).
- Constraints (what NOT to do, safety gates).
- Context (pointers to files/skills, not dumps).
- Output contract (format, files to touch, definition of done).
- Verification (how to prove it works).
- Few-shot examples for anything subtle.

Rules:
- Prompt quality compounds through skills: a fixed prompt is a snapshot; a skill is a living prompt that improves every time the agent hits a pitfall.
- Match the prompt to the MODEL CLASS: a reasoning-heavy model wants a different prompt than a fast model (his /PLAN rebuild).
- The model is the CPU, the prompt is the program — and skills are your program library."#;

const AGENTS_LEARNING: &str = r#"AGENTS THAT ACTUALLY LEARN (Agent Experts)

Thesis: "Agent Experts: Finally, Agents That ACTUALLY Learn." The frontier after harness engineering is agents that improve themselves: capture what worked, store it durably, and apply it on the next task without being told again.

The learning loop:
1. OBSERVE: after each task, capture the outcome — what worked, what failed, the pitfall, the fix.
2. STORE: write it to durable memory (notes, lessons) or distill it into a skill (the promotion path: working solution → SKILL.md).
3. INJECT: on the next relevant task, the memory/skill is injected into context (relevance-matched, not everything).
4. APPLY: the agent uses the learned pattern; failures become new lessons; the loop compounds.
- This turns a stateless model into a stateful agent: same model, growing competence.

Implementation notes:
- Memory layers: short-term (session), long-term (notes), skills (procedures), and the meta-layer (what to remember). 4-layer memory.
- Relevance injection beats brute-force context: search the memory for what's relevant to THIS task (TF-IDF/semantic), inject only that.
- Self-review: after heavy turns, auto-review and record a durable lesson (ByteAi's auto-review pattern).
- The Library meta-skill distributes learned skills across projects — learning at the org level, not just the agent level.

Rule: an agent that doesn't learn is a calculator; an agent that learns is a colleague. Build the capture → store → inject → apply loop into every harness.

ByteAi mapping: memory tool (notes), memsearch tool (TF-IDF relevance injection), skills tool (capture → SKILL.md), review/auto-review (lessons) — the full learning loop is native."#;

/// Full knowledge base: topic key → body text.
const BODIES: &[(&str, &str)] = &[
    ("overview", OVERVIEW),
    ("harness_engineering", HARNESS_ENGINEERING),
    ("skills_system", SKILLS_SYSTEM),
    ("agent_sandboxes", AGENT_SANDBOXES),
    ("multi_agent", MULTI_AGENT),
    ("model_selection", MODEL_SELECTION),
    ("security", SECURITY),
    ("observability", OBSERVABILITY),
    ("context_engineering", CONTEXT_ENGINEERING),
    ("planning", PLANNING),
    ("local_models", LOCAL_MODELS),
    ("agent_threads", AGENT_THREADS),
    ("software_factory", SOFTWARE_FACTORY),
    ("prompt_engineering", PROMPT_ENGINEERING),
    ("agents_learning", AGENTS_LEARNING),
];

pub struct DanMethodologyTool;

impl Tool for DanMethodologyTool {
    fn name(&self) -> &'static str {
        "dan_methodology"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "dan_methodology".into(),
            description: "IndyDevDan's agentic-engineering methodology (distilled from all 51 of his videos): harness engineering, skills, sandboxes, multi-agent teams, model stacking, security, observability, context engineering, /PLAN, local models, agent threads, the software factory, prompt engineering, agents that learn. Query a topic to get actionable guidance you can apply to ByteAi right now. Call with no args for the full overview; call with `topic` for a deep section; call with `query` for free-text lookup across all topics.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "enum": ["overview", "harness_engineering", "skills_system", "agent_sandboxes", "multi_agent", "model_selection", "security", "observability", "context_engineering", "planning", "local_models", "agent_threads", "software_factory", "prompt_engineering", "agents_learning"],
                        "description": "Topic to load. Omit for the full overview."
                    },
                    "query": {
                        "type": "string",
                        "description": "Free-text query; matches against topic titles+descriptions and returns the best-matching sections."
                    }
                }
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();

            let topic = args.get("topic").and_then(|t| t.as_str()).unwrap_or("").to_lowercase();
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("").to_lowercase();

            if topic.is_empty() && query.is_empty() {
                // Full overview + topic index.
                out.push_str("DAN METHODOLOGY — @indydevdan (agentic engineering), distilled from all 51 videos.\n\n");
                out.push_str(OVERVIEW);
                out.push_str("\n\nTOPICS (call with {\"topic\": \"<key>\"}):\n");
                for (k, d) in TOPICS {
                    out.push_str(&format!("  {k:<24} {d}\n"));
                }
                out.push_str("\nOr pass {\"query\": \"...\"} for free-text lookup.\n");
                return ok_outcome("", "dan_methodology", out, started.elapsed().as_millis() as u64);
            }

            if !topic.is_empty() {
                match BODIES.iter().find(|(k, _)| *k == topic) {
                    Some((_, body)) => {
                        out.push_str(&format!("DAN METHODOLOGY · {topic}\n\n{body}\n"));
                    }
                    None => {
                        let valid: Vec<&str> = TOPICS.iter().map(|(k, _)| *k).collect();
                        out.push_str(&format!(
                            "Unknown topic {topic:?}. Valid topics: {}. Or use {{\"query\": \"...\"}}.\n",
                            valid.join(", ")
                        ));
                        return ok_outcome("", "dan_methodology", out, started.elapsed().as_millis() as u64);
                    }
                }
                return ok_outcome("", "dan_methodology", out, started.elapsed().as_millis() as u64);
            }

            // Free-text query: score each topic by token overlap with the query.
            let q_tokens: Vec<&str> = query
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.len() > 2)
                .collect();
            let mut scored: Vec<(usize, &str, &str)> = Vec::new();
            for (k, d) in TOPICS {
                let hay = format!("{k} {d}").to_lowercase();
                let hits = q_tokens.iter().filter(|t| hay.contains(**t)).count();
                if hits > 0 {
                    scored.push((hits, k, d));
                }
            }
            scored.sort_by_key(|b| std::cmp::Reverse(b.0));
            if scored.is_empty() {
                out.push_str(&format!(
                    "No topic matched {query:?}. Try one of: {}\n",
                    TOPICS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
                ));
            } else {
                out.push_str("DAN METHODOLOGY · matching sections\n\n");
                for (hits, k, _) in scored.iter().take(3) {
                    out.push_str(&format!("=== {k} (match {hits}) ===\n"));
                    if let Some((_, body)) = BODIES.iter().find(|(bk, _)| bk == k) {
                        out.push_str(body);
                        out.push('\n');
                    }
                    out.push('\n');
                }
            }
            ok_outcome("", "dan_methodology", out, started.elapsed().as_millis() as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tool: &DanMethodologyTool, args: Value) -> ToolOutcome {
        // execute() is async; run it on a minimal tokio runtime-free block.
        let fut = tool.execute(args);
        tokio::runtime::Runtime::new().unwrap().block_on(fut)
    }

    #[test]
    fn overview_returns_index() {
        let tool = DanMethodologyTool;
        let o = run(&tool, json!({}));
        assert!(o.ok);
        assert!(o.output.contains("harness_engineering"));
        assert!(o.output.contains("multi_agent"));
        assert!(o.output.contains("software_factory"));
        assert!(o.output.contains("TOPICS"));
    }

    #[test]
    fn topic_returns_section() {
        let tool = DanMethodologyTool;
        let o = run(&tool, json!({"topic": "planning"}));
        assert!(o.ok);
        assert!(o.output.contains("/PLAN"));
        assert!(o.output.contains("PLANNING"));
        let o2 = run(&tool, json!({"topic": "security"}));
        assert!(o2.ok);
        assert!(o2.output.contains("BASH"));
    }

    #[test]
    fn unknown_topic_returns_valid_list() {
        let tool = DanMethodologyTool;
        let o = run(&tool, json!({"topic": "nope"}));
        assert!(o.ok);
        assert!(o.output.contains("Unknown topic"));
        assert!(o.output.contains("planning"));
    }

    #[test]
    fn query_scores_by_token_overlap() {
        let tool = DanMethodologyTool;
        let o = run(&tool, json!({"query": "sandbox execute safely"}));
        assert!(o.ok);
        assert!(o.output.contains("agent_sandboxes"));
    }

    #[test]
    fn all_bodies_have_known_topics() {
        assert_eq!(BODIES.len(), TOPICS.len(), "every topic has a body");
        for (k, _) in TOPICS {
            assert!(
                BODIES.iter().any(|(bk, _)| bk == k),
                "missing body for topic {k}"
            );
        }
        // Every body is non-trivial.
        for (k, b) in BODIES {
            assert!(b.len() > 500, "body for {k} too short: {}", b.len());
        }
    }
}
