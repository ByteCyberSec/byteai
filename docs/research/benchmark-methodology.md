# ByteAi Benchmark Methodology

> Phase 0 deliverable. Defines reproducible benchmarks for ByteAi vs reference projects.
> All benchmarks must record: hardware, OS version, model, provider, date, agent version, settings.

## 1. Performance Benchmarks

### Hardware / Software Recording
- CPU model, cores, RAM, disk type, GPU (if any)
- OS version, kernel
- All agent versions (jcode, oh-my-pi, hermes, claude, codex, opencode, pi)
- Model name, provider, context window, temperature
- Date, time, repo commit hash

### 1a. Startup Latency
- **Method**: time-to-first-prompt-acceptance (from process start to first input field ready)
- **Tools**: `hyperfine` (Rust), 10 runs, warm + cold cache
- **Compare against**: jcode (14ms claimed), hermes, claude, codex, opencode, pi

### 1b. Idle RAM
- **Method**: `ps -o rss,pid` after process is idle for 30s. Measure PSS on Linux, `vmmap` on macOS.
- **Scenarios**: 1 session, 5 sessions, 10 sessions (where applicable)
- **Compare against**: jcode (27.8MB/117MB claimed), all other agents

### 1c. RAM per Additional Session
- **Method**: Start N sessions, measure PSS, subtract baseline, divide by N.
- **Target**: <20 MB per session for ByteAi.

### 1d. Search Latency
- **Method**: Time to search a 10K-file repo for a known string, a regex, and a symbol.
- **Tools**: `hyperfine`, 10 runs.
- **Compare against**: ripgrep baseline, jcode fuzzy, hermes search, oh-my-pi natives.

### 1e. File Read Efficiency
- **Method**: Measure tokens/characters returned for a "read and understand" task on a 1,500-line file. Compare charging the model for the whole file vs progressive disclosure.
- **Target**: <30% of whole-file tokens for the same task success rate.

## 2. Engineering Benchmarks

### 2a. Edit Success Rate
- **Method**: 50 standard edit tasks (single-line, multi-line, multi-file, ambiguous match, whitespace-sensitive). Count first-attempt success, retries, total attempts.
- **Compare against**: all agents.
- **Tool**: oh-my-pi's `typescript-edit-benchmark` package is a starting point.

### 2b. Tool Retries
- **Method**: Count tool call retries per task. Classify by failure reason (syntax, timeout, permission, etc.).
- **Target**: <1.1 retries per successful task.

### 2c. Tokens per Task
- **Method**: Record total input + output tokens per task.
- **Target**: metric for optimization, not a hard number.

### 2d. Time per Task
- **Method**: Wall-clock time from task submission to verified completion.
- **Target**: compare against all agents.

## 3. Task-Based Quality Evals (16 tasks, from spec §39)

### Task List
1. Single-file bug fix
2. Multi-file bug fix
3. Unknown codebase (first-time interaction)
4. Safe refactor (behavior-preserving)
5. API contract change (backward-compatible)
6. Dependency upgrade (major version)
7. Test failure diagnosis + fix
8. Runtime crash diagnosis + fix
9. Performance regression diagnosis + fix
10. Database migration (schema change, zero-downtime)
11. Frontend bug (CSS/state)
12. Backend bug (logic/async)
13. Concurrency bug (race/deadlock)
14. Security issue (injection/XSS/CSRF)
15. Large repository search (find and fix across 50+ files)
16. Cross-session continuation (start task, end session, resume in new session)

### Scoring
Each task scored on:
- **Correctness** (0-10): does the fix actually work?
- **Time** (seconds)
- **Tokens** (input + output)
- **Tool calls** (count)
- **Human intervention** (0 = none, 1 = minor steering, 2 = significant direction, 3 = failed without human)
- **Regressions** (0 = none, -1 per regression)

### Minimum passing score
- Correctness >= 7/10
- Human intervention <= 1
- Zero regressions

## 4. Ablation Benchmarks (spec §40)

Every major "smart" feature is benchmarked with and without:
- Memory on/off
- Semantic retrieval on/off
- Reviewer on/off
- Subagents on/off
- LSP on/off
- Skills on/off
- Smart routing on/off (vs "always use strongest model")
- Context compaction on/off

If a feature does not produce measurable improvement (correctness, time, or tokens) on at least 2 of the 16 task classes, it is removed or made opt-in.

## 5. Benchmark Environment

- Primary: macOS 14.5 (current host), Apple Silicon
- Secondary (when available): Linux x86_64
- Models: deepseek-v4-flash (current), plus at least one of: claude-3.5-sonnet, gpt-4o, gemini-2.0-flash — for cross-model comparison
- Provider: local OmniRoute (localhost:20128) for local models, plus any cloud provider for hosted models
- Each benchmark run publishes: `benchmarks/<date>-<agent>-<model>/results.json`

## 6. Local Agents Available for Comparison

Installed at `/Users/ingfix/`:
- jcode (via ~/.local/bin/jcode)
- pi (via ~/.local/bin/pi)
- hermes (via ~/.local/bin/hermes)
- claude (via cmux shim)
- codex (via cmux shim)
- opencode (via ~/.local/bin/opencode)
- strix (via ~/.strix/bin/strix)
- OmniRoute at localhost:20128 (557 models)

## 7. Benchmark script structure

```
benchmarks/
├── run.py               — main harness: selects agent, runs task, records metrics
├── tasks/               — task definitions (16 task dirs, each with README.md, expected output, repo fixture)
├── results/             — timestamped result JSON files
├── analyze.py           — compare results, generate tables
└── methodology.md       — this file
```