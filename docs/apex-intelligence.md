# APEX Intelligence Engine — /Ideas and /Github Commands

ByteAI's `/Ideas` and `/Github` commands implement the full APEX intelligence
engine: evidence-based idea discovery, capability discovery, compatibility
evaluation, skill and harness management, continuous capability tracking, and
just-in-time tool acquisition.

## Compatibility Engine

Every candidate repository evaluated by `/Github` receives:

| Field | Values |
|-------|--------|
| APEX compatibility | 0–100 |
| Current project compatibility | 0–100 |
| Integration complexity | Low / Medium / High |
| Performance impact | Positive / Neutral / Negative |
| Security risk | Low / Medium / High |
| Maintenance risk | Low / Medium / High |
| License | SPDX identifier |
| Recommendation | ADOPT / ADAPT / LEARN FROM / REJECT |

**ADOPT vs LEARN** — Never assume installing a repository is the best solution.

- **ADOPT** — Use directly.
- **ADAPT** — Integrate selected components.
- **LEARN FROM** — Reimplement the architectural idea.
- **REJECT** — Not worth using.

## Skill Discovery

When `/Github skills` finds a useful skill, inspect:
- triggers
- instructions
- assumptions
- tools
- dependencies
- compatibility
- quality
- overlap with existing skills

Then compare against existing APEX skills. Possible actions:
**ADD** / **MERGE** / **UPGRADE** / **REPLACE** / **IGNORE**

Do not accumulate duplicate skills. Build the strongest unified skill rather
than loading all candidates blindly.

## Harness Discovery

When searching agent harnesses, compare:
- startup, memory, context handling, edit system, search, LSP, DAP, skills,
  MCP, subagents, model routing, providers, sandboxing, security, TUI,
  tests, maintenance, license

Store findings in `<data>/intelligence/harnesses/` (mirrored to
`.apex/intelligence/harnesses/`).

## Continuous Capability Graph

Maintained under `<data>/intelligence/capabilities.md`:

```
Capability
├── Current implementation
├── Available skills
├── Available tools
├── Available harnesses
├── Candidate improvements
└── Benchmark history
```

## Capability Gap Detection

Before building an idea, compare required capabilities vs available
capabilities. Automatically trigger targeted GitHub discovery for missing
capabilities instead of searching everything.

## Just-In-Time Capability Acquisition

Core APEX principle: do not install thousands of skills ahead of time.

```
TASK → CAPABILITY REQUIREMENTS → CAPABILITY GAP → SEARCH → EVALUATE
→ LOAD / ADAPT → BUILD
```

## Tool Benchmarking

When multiple candidates solve the same capability, benchmark them on a
representative mini-task. Measure: setup complexity, latency, reliability,
API quality, memory, dependency weight.

## GitHub Intelligence Memory

Remember previous evaluations. Do not repeatedly research the same
repository. Store: repo, commit evaluated, date, purpose, score, strengths,
weaknesses, license, compatibility, benchmark, decision.

## Update Mode

`/Github update` checks whether:
- installed skills improved
- harnesses changed
- new releases exist
- important bugs were fixed
- better alternatives emerged
- security problems were reported

## Improve Mode

`/Github improve` searches specifically for technology that can make the
coding agent itself stronger. Research areas: reasoning harnesses, context
management, memory, retrieval, editing, LSP, debugging, subagents, planning,
verification, testing, browser tools, MCP, sandboxing, model routing, prompt
caching, token efficiency, TUI, observability, security.

## Autonomous Discovery During Builds

During `/Ideas` builds, ByteAI may internally run GitHub intelligence when:
- capability is missing
- implementation is failing
- existing dependency is poor
- a specialized skill would help
- a mature library could replace unnecessary custom code
- security tooling / deployment tooling / better test infrastructure is needed

Do not interrupt the user for every discovery. Use the best safe option
automatically unless adoption has licensing, security, major architecture,
or paid-service implications.

## Intelligence Storage

All findings are stored under two roots:

1. **Persistent**: `<data_dir>/intelligence/` — survives across projects
   (macOS: `~/Library/Application Support/byteai/intelligence/`)
2. **Local**: `.apex/intelligence/` in the project working directory —
   travels with the project

Subdirectories:
- `ideas/` — `/Ideas` discovery results, research, build plans
- `repos/` — repository evaluations, compatibility scores
- `harnesses/` — harness evaluations
- `improvements/` — `/Github improve` ranked improvements
- `capabilities.md` — the continuous capability graph

## `/Ideas` Complete Workflow

```
/Ideas
↓
SEARCH INTERNET (multiple angles, problem-mining phrases)
↓
MINE REAL PROBLEMS (never invent demand)
↓
VALIDATE DEMAND (evidence, not imagination)
↓
ANALYZE COMPETITION (existing solutions, weaknesses, pricing)
↓
TOP 5 UNIQUE IDEAS (with ByteAI Opportunity Scores)
↓
USER SELECTS IDEA
↓
DEEP RESEARCH (product, market, technology, risk)
↓
CAPABILITY GAP ANALYSIS (via /Github current)
↓
BEST SKILLS / TOOLS / LIBRARIES (discovery + evaluation)
↓
ARCHITECTURE
↓
PHASE-BY-PHASE OR FULL AUTONOMOUS BUILD
↓
IMPLEMENT → TEST → DEBUG → REVIEW → SECURE → DEPLOY → VERIFY → SHIP
```

## `/Github` Complete Workflow

```
/Github
↓
SELECT TARGET
↓
ANALYZE BYTEAI / CURRENT PROJECT
↓
IDENTIFY CAPABILITY GAPS
↓
SEARCH GITHUB (multiple queries, deep inspection)
↓
INSPECT CANDIDATES (fetch README, evaluate source, not stars)
↓
CHECK HEALTH (maintenance, recency, contributors, issues, tests)
↓
CHECK LICENSE
↓
CHECK SECURITY
↓
COMPARE (benchmark when useful)
↓
COMPATIBILITY SCORE (APEX 0–100, project 0–100, complexity, risk)
↓
ADOPT / ADAPT / LEARN / REJECT
↓
UPDATE CAPABILITY GRAPH
↓
INTEGRATE WHEN USEFUL
↓
VERIFY
```

## Critical Behavior

ByteAI must develop a habit of asking internally:

- Is there already excellent open-source work solving this?
- Is there a better skill for this?
- Is there a better harness for this?
- Is there a better tool for this?
- Can I reuse a proven implementation instead of wasting time?
- Would a custom implementation actually be better?
- What does the evidence say?

Never reinvent merely for pride. Never add dependencies merely for
convenience. Never choose technology because it is fashionable.

Choose based on: correctness, speed, maintainability, security, compatibility,
developer experience, performance, license, maturity, production readiness.

## The Ultimate Goal

`/Ideas` gives ByteAI the ability to discover **what should be built**.
`/Github` gives ByteAI the ability to discover **the best way to build it**.

Combined:

```
INTERNET + COMMUNITY INTELLIGENCE + GITHUB + SKILLS + TOOLS + MEMORY
+ MULTI-AGENT EXECUTION + AUTONOMOUS ENGINEERING
= SELF-EXPANDING CODING AGENT
```

ByteAI should become more capable as it encounters new problems without
turning itself into a bloated collection of unused tools. **Discover just in
time. Learn permanently. Load selectively. Benchmark alternatives. Build
autonomously. Verify everything. Ship production-ready software.**