//! `ideas` — /Ideas: evidence-based product-idea discovery, validation,
//! planning and building (the GitHub-intelligence workflow).
//!
//! Doctrine (see docs/apex-intelligence.md):
//!   * Mine real problems from the internet — never invent demand. Search
//!     multiple angles: complaints, "I wish there was…", workarounds,
//!     feature requests, spreadsheets-still-in-use, alternatives, pricing.
//!   * Validate demand + competition with the LLM before recommending.
//!   * Return the Top 5 genuinely different opportunities, each with a
//!     ByteAI Opportunity Score and an evidence trail.
//!   * Prefer recent sources; record dates.
//!   * Persist every finding under <data>/intelligence/ideas/ + agent memory
//!     so /Github and future /Ideas runs build on prior evidence.
//!
//! Reuses ByteAI's existing capabilities: `websearch` for discovery,
//! `fetch` for reading sources, the configured provider for synthesis,
//! agent memory for persistence, and the `plan` tool format for builds.

use std::time::Instant;

use apex_memory::Kind;
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::websearch::WebSearchTool;
use crate::{BoxFuture, Tool, ToolContext, ok_outcome};

/// Max search queries per discovery run (keep it fast + bounded).
const MAX_QUERIES: usize = 8;
/// Evidence budget fed to the synthesizer (chars).
const EVIDENCE_BUDGET: usize = 16_000;
/// Concurrent web searches.
const CONCURRENCY: usize = 4;

/// The 19 /Ideas menu categories → focus keywords.
fn category_for_choice(c: &str) -> Option<&'static str> {
    Some(match c.trim() {
        "1" => "startup opportunities trending right now",
        "2" => "AI products",
        "3" => "SaaS",
        "4" => "developer tools",
        "5" => "B2B software",
        "6" => "consumer apps",
        "7" => "mobile apps",
        "8" => "browser extensions",
        "9" => "automation tools",
        "10" => "APIs and infrastructure",
        "11" => "open source products",
        "12" => "problems people actively complain about",
        "13" => "underserved niche communities",
        "14" => "fast growing trends",
        "15" => "unique software ideas with low competition",
        _ => return None,
    })
}

/// Build the multi-angle problem-mining query set for a focus.
/// Follows the doctrine: never one search, always several angles + sources.
fn build_queries(focus: &str) -> Vec<String> {
    let f = focus.trim();
    let f = category_for_choice(f).unwrap_or(f);
    let mut qs: Vec<String> = Vec::new();
    if f.is_empty() {
        // "Surprise me" — generic opportunity mining.
        qs.extend([
            "startup ideas 2026".to_string(),
            "problems people want software for 2026".to_string(),
            "\"I wish there was\" a tool for".to_string(),
            "underserved software niche".to_string(),
            "trending tech 2026".to_string(),
            "software people are asking for on reddit".to_string(),
            "site:news.ycombinator.com what are people building".to_string(),
            "site:producthunt.com new product ideas".to_string(),
        ]);
        return qs;
    }
    // Problem-mining phrases × focus.
    qs.push(format!("{f} problem"));
    qs.push(format!("{f} complaints"));
    qs.push(format!("{f} \"I wish there was\""));
    qs.push(format!("{f} \"is there a tool\""));
    qs.push(format!("{f} manual process spreadsheet"));
    qs.push(format!("{f} how to automate"));
    qs.push(format!("{f} too expensive workaround"));
    qs.push(format!("{f} alternative"));
    // Source-targeted (best communities for the niche).
    qs.push(format!("site:reddit.com {f}"));
    qs.push(format!("site:news.ycombinator.com {f}"));
    qs.push(format!("site:github.com {f}"));
    qs.push(format!("site:producthunt.com {f}"));
    qs.truncate(MAX_QUERIES.max(qs.len().min(MAX_QUERIES)));
    qs
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

async fn search_one(query: String, max: u64) -> String {
    let tool = WebSearchTool;
    let o = tool.execute(json!({ "query": query, "max": max })).await;
    o.output
}

/// Run queries with bounded concurrency, return outputs in order.
async fn run_queries(queries: Vec<String>, max: u64) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    let mut iter = queries.into_iter();
    for _ in 0..CONCURRENCY {
        if let Some(q) = iter.next() {
            set.spawn(search_one(q, max));
        }
    }
    let mut out = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(text) = res {
            out.push(text);
        }
        if let Some(q) = iter.next() {
            set.spawn(search_one(q, max));
        }
    }
    out
}

pub struct IdeasTool {
    pub ctx: ToolContext,
}

impl IdeasTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }

    fn menu(&self) -> String {
        [
            "# /Ideas — what should ByteAI discover?",
            "",
            " 1. Best opportunities right now      10. APIs / infrastructure",
            " 2. AI products                       11. Open-source products",
            " 3. SaaS                              12. Problems people complain about",
            " 4. Developer tools                   13. Underserved niches",
            " 5. B2B software                      14. Fast-growing trends",
            " 6. Consumer apps                     15. Unique ideas, low competition",
            " 7. Mobile apps                       16. A specific industry",
            " 8. Browser extensions                17. Match my skills/interests",
            " 9. Automation tools                  18. Surprise me",
            "                                    19. Custom search",
            "",
            "Use: /ideas <category>  (e.g. /ideas AI + SaaS, /ideas developer tools,",
            "     /ideas problems small businesses complain about, /ideas healthcare automation)",
            "     /ideas research <idea>   — deep-research one idea before building",
            "     /ideas build <idea>      — spec + phase plan, then build",
            "     /ideas status            — list saved ideas",
            "",
        ]
        .join("\n")
    }

    fn discover(&self, focus: &str) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        let focus = focus.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let queries = build_queries(&focus);
            let results = run_queries(queries.clone(), 5).await;

            // Assemble evidence (bounded).
            let mut evidence = String::new();
            for (i, r) in results.iter().enumerate() {
                if evidence.chars().count() > EVIDENCE_BUDGET {
                    break;
                }
                evidence.push_str(&format!("## search {}: {}\n\n", i + 1, queries.get(i).cloned().unwrap_or_default()));
                let body: String = r.chars().take(EVIDENCE_BUDGET / results.len().max(1)).collect();
                evidence.push_str(&body);
                evidence.push('\n');
            }

            if evidence.trim().is_empty() {
                out.push_str(&format!(
                    "# /Ideas — no evidence found for \"{focus}\"\n\nNo web results returned. Try a broader focus, or use /ideas menu to pick a category. (websearch: DuckDuckGo Lite, no key needed.)"
                ));
                return ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64);
            }

            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let system = [
                "You are ByteAi's /Ideas engine. You discover product opportunities from REAL internet evidence, never from imagination alone.",
                "Rules:",
                "1. Mine the evidence for repeated, painful, unsolved problems — not generic trends.",
                "2. Validate each idea: who has it, how often, current solutions, competitors, complaints about competitors, workarounds, willingness-to-pay indicators.",
                "3. Return the TOP FIVE genuinely DIFFERENT opportunities. Never five variations of the same product.",
                "4. Prefer recent evidence (the search date is given). If a problem looks stale, say so.",
                "5. Competition does not invalidate an idea — existing businesses prove demand; identify the wedge.",
            ].join("\n");
            let user = format!(
                "Today: {today}\nFocus: {focus}\n\nBelow is raw web-search evidence (multiple angles: complaints, wish-list, workarounds, alternatives, communities). Use it.\n\n=== EVIDENCE ===\n{evidence}\n=== END EVIDENCE ===\n\nFor EACH of the top five ideas output EXACTLY this block:\n\n#1 — IDEA NAME\n\nConcept:\nOne sentence.\n\nProblem:\nWhat real problem it solves.\n\nTarget users:\nWho needs it.\n\nEvidence:\nWhat the search results show (name sources/URLs when visible).\n\nCurrent workaround:\nHow users solve it today.\n\nCompetitors:\nWhat already exists.\n\nCompetitor weaknesses:\nWhat users dislike / what is missing.\n\nOur opportunity:\nWhy this could be better.\n\nUnique angle:\nWhat makes this version different.\n\nMVP:\nSmallest useful version.\n\nFuture expansion:\nWhat it could become.\n\nMonetization:\nHow it makes money.\n\nTechnical difficulty:\n1–10\n\nCompetition:\nLow / Medium / High\n\nDemand confidence:\n1–10\n\nBuild feasibility:\n1–10\n\nRevenue potential:\n1–10\n\nUniqueness:\n1–10\n\nByteAI Opportunity Score:\nXX / 100\n\nThen a final 'Opportunity Ranking' section explaining your evidence-based ordering (pain intensity, demand evidence, willingness to pay, competition, differentiation, build feasibility, distribution, timing, expansion, revenue)."
            );
            let synth = self_llm(&ctx, &system, &user, 6000).await.unwrap_or_else(|e| format!("Synthesis failed: {e}"));

            out.push_str(&synth);
            out.push('\n');

            // Persist.
            let file = format!("{}-{}.md", today, slug(&focus));
            let full = format!(
                "---\ntype: ideas\nfocus: {focus}\ndate: {today}\nsources: {}\n---\n\n{focus}\n\n{out}\n",
                queries.len()
            );
            match self_persist(&ctx, &file, &full) {
                Ok(_) => out.push_str(&format!("\n[saved] {}/ideas/{file}\n", ctx.data_dir.join("intelligence").display())),
                Err(e) => out.push_str(&format!("\n[save failed: {e:#}]\n")),
            }
            // Short memory note.
            let mem_body: String = synth.chars().take(1500).collect();
            out.push_str(&self_remember(
                &ctx,
                &format!("Ideas {}", slug(&focus)),
                &mem_body,
                &["ideas", "opportunity"],
            ));
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));

            ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64)
        })
    }

    fn research(&self, idea: &str) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        let idea = idea.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            // Deep search: multiple angles on the single idea.
            let queries = vec![
                format!("{idea}"),
                format!("{idea} reddit"),
                format!("{idea} forum complaints"),
                format!("{idea} software"),
                format!("{idea} alternative"),
                format!("{idea} workaround"),
                format!("{idea} feature request"),
                format!("{idea} github"),
                format!("{idea} pricing"),
                format!("{idea} reviews"),
            ];
            let results = run_queries(queries.clone(), 5).await;
            let mut evidence = String::new();
            for (i, r) in results.iter().enumerate() {
                if evidence.chars().count() > EVIDENCE_BUDGET {
                    break;
                }
                evidence.push_str(&format!("## search {}: {}\n\n", i + 1, queries[i]));
                evidence.push_str(&r.chars().take(EVIDENCE_BUDGET / results.len().max(1)).collect::<String>());
                evidence.push('\n');
            }
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let system = "You are ByteAi's /Ideas deep-research engine. Produce a rigorous, evidence-based deep-dive on ONE product idea before it is built. Cover: Product (core user journey, must-have vs nice-to-have features, UX, integrations), Market (competitors, pricing, positioning, gaps), Technology (best stack, APIs, frameworks, open-source projects, infrastructure, deployment), Risk (security, privacy, legal, licensing, platform dependency, API costs, scalability, abuse). Then give a recommended implementation plan (phases). Be concrete and cite evidence from the searches.";
            let user = format!(
                "Today: {today}\nIdea: {idea}\n\n=== EVIDENCE ===\n{evidence}\n=== END EVIDENCE ===\n\nProduce the deep-research report + recommended implementation plan."
            );
            let synth = self_llm(&ctx, system, &user, 6000)
                .await
                .unwrap_or_else(|e| format!("Research failed: {e}"));
            out.push_str(&synth);

            let file = format!("research-{}-{}.md", today, slug(&idea));
            if let Err(e) = self_persist(&ctx, &file, &out) {
                out.push_str(&format!("\n[save failed: {e:#}]"));
            } else {
                out.push_str(&format!("\n[saved] {}/ideas/{file}\n", ctx.data_dir.join("intelligence").display()));
            }
            out.push_str(&self_remember(
                &ctx,
                &format!("Research {}", slug(&idea)),
                &synth.chars().take(1500).collect::<String>(),
                &["ideas", "research"],
            ));
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));
            ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64)
        })
    }

    fn build(&self, idea: &str) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        let idea = idea.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let system = "You are ByteAi's /Ideas build planner. Turn the selected idea into a concrete, production-ready implementation plan. Use ByteAI's own capabilities where they fit: existing skills, plan tool, verify, review, gates, spawn (subagents), route/council for decisions. Output a phase-by-phase plan (SPEC → ARCHITECTURE → SETUP → IMPLEMENT → TEST → DEBUG → REVIEW → SECURITY → OPTIMIZE → DEPLOY → VERIFY → SHIP). For each phase: goal, concrete files, key decisions, and how to verify it (tests). Then a 'Stack' section recommending: coding model, skills, tools, MCP servers, libraries, database, testing stack, deployment target — chosen FOR THIS PROJECT, not a one-size-fits-all stack. Do NOT invent demand; build what the evidence showed.";

            // Pull any saved research on this idea as context.
            let mut prior = String::new();
            if let Ok(mem) = apex_memory::Memory::open(&ctx.data_dir.join("memory"))
                && let Ok(entries) = mem.search(&idea, Some(Kind::Note), 3) {
                    for e in entries {
                                                prior.push_str(&e.body);
                                                prior.push('\n');
                                            }
                }
            let prior_block = if prior.is_empty() {
                "(no saved research found — plan from the idea description)".to_string()
            } else {
                format!("Prior research on this idea:\n{prior}")
            };

            let user = format!(
                "Today: {today}\nIdea: {idea}\n\n{prior_block}\n\nProduce the implementation plan. Use ByteAI's existing tooling in the plan (plan, verify, review, gates, spawn, skills, git, sandbox, improve). Output as a plain checklist so it can be loaded into the `plan` tool, then the phase-by-phase narrative."
            );
            let synth = self_llm(&ctx, system, &user, 6000)
                .await
                .unwrap_or_else(|e| format!("Plan failed: {e}"));
            out.push_str(&synth);

            let file = format!("build-{}-{}.md", today, slug(&idea));
            if let Err(e) = self_persist(&ctx, &file, &out) {
                out.push_str(&format!("\n[save failed: {e:#}]"));
            } else {
                out.push_str(&format!("\n[saved] {}/ideas/{file}\n", ctx.data_dir.join("intelligence").display()));
            }
            out.push_str("\n[build] To execute: continue this session — the agent will load the plan and build it phase by phase (or run in full-autonomous mode).");
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));
            ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64)
        })
    }

    fn status(&self) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = Instant::now();
            let dir = ctx.data_dir.join("intelligence").join("ideas");
            let mut out = String::new();
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    let mut files: Vec<_> = entries.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
                    files.sort();
                    if files.is_empty() {
                        out.push_str("No saved ideas yet. Run /ideas <focus> to discover.\n");
                    } else {
                        out.push_str(&format!("Saved ideas ({})\n", files.len()));
                        for f in files {
                            out.push_str(&format!("  {f}\n"));
                        }
                    }
                }
                Err(_) => out.push_str("No saved ideas yet. Run /ideas <focus> to discover.\n"),
            }
            ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64)
        })
    }
}

/// Free functions so the futures can own a cloned context (no self borrows
/// across await points).
fn self_llm(
    ctx: &ToolContext,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> BoxFuture<'static, Result<String, String>> {
    let ctx = ctx.clone();
    let system = system.to_string();
    let user = user.to_string();
    Box::pin(async move {
        let client = ctx
            .client
            .clone()
            .ok_or_else(|| "no provider client — configure a provider in config.toml".to_string())?;
        let model = if ctx.default_model.is_empty() {
            "deepseek-v4-flash"
        } else {
            &ctx.default_model
        };
        let sys = apex_types::Message::system(&system);
        let usr = apex_types::Message::user(&user);
        client
            .chat(model, &[sys, usr], &[], Some(max_tokens))
            .await
            .map(|(t, _, _)| t)
            .map_err(|e| format!("{e:#}"))
    })
}

fn self_persist(ctx: &ToolContext, rel: &str, content: &str) -> std::io::Result<()> {
    let roots = [
        ctx.data_dir.join("intelligence"),
        std::env::current_dir()
            .unwrap_or_default()
            .join(".apex")
            .join("intelligence"),
    ];
    for root in roots {
        let dir = root.join("ideas");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(rel), content)?;
    }
    Ok(())
}

fn self_remember(ctx: &ToolContext, title: &str, body: &str, tags: &[&str]) -> String {
    match apex_memory::Memory::open(&ctx.data_dir.join("memory")) {
        Ok(mut mem) => match mem.upsert(
            Kind::Note,
            title,
            body,
            &tags.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            None,
        ) {
            Ok(id) => format!("\n[memory] saved #{id} \"{title}\""),
            Err(e) => format!("\n[memory] write failed: {e:#}"),
        },
        Err(e) => format!("\n[memory] unavailable: {e:#}"),
    }
}

impl Tool for IdeasTool {
    fn name(&self) -> &'static str {
        "ideas"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "ideas".into(),
            description: "/Ideas — evidence-based product-idea discovery, validation, planning and building. \
Mines real problems from the internet (multiple search angles), validates demand with the LLM, returns the Top 5 with \
ByteAI Opportunity Scores, persists findings to intelligence/ideas/ + memory. Actions: menu, discover, research, build, status. \
Input: {action, focus}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["menu", "discover", "research", "build", "status"], "description": "What to do" },
                    "focus": { "type": "string", "description": "Category or query (e.g. 'AI + SaaS', 'developer tools', 'problems small businesses complain about', or a specific idea)" }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("menu")
            .to_string();
        let focus = args
            .get("focus")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match action.as_str() {
            "menu" => {
                let out = self.menu();
                let started = Instant::now();
                Box::pin(async move { ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64) })
            }
            "discover" => self.discover(&focus),
            "research" => self.research(&focus),
            "build" => self.build(&focus),
            "status" => self.status(),
            other => {
                let out = format!("ERROR: unknown action {other:?}\n{}", self.menu());
                let started = Instant::now();
                Box::pin(async move { ok_outcome("", "ideas", out, started.elapsed().as_millis() as u64) })
            }
        }
    }
}
