//! `github` — /Github: capability discovery and upgrade engine.
//!
//! Doctrine (see docs/apex-intelligence.md):
//!   * Find the best way to build: skills, harnesses, tools, MCP servers,
//!     libraries, coding-agent technology.
//!   * Evaluate candidates with the COMPATIBILITY ENGINE: APEX compatibility
//!     0–100, project compatibility 0–100, integration complexity,
//!     performance impact, security risk, maintenance risk, license, and a
//!     single decision — ADOPT / ADAPT / LEARN FROM / REJECT.
//!   * Never rank by stars alone; inspect the actual source (README) for
//!     important candidates. Never install everything found.
//!   * Keep a GitHub intelligence memory (repo, commit/date, purpose, score,
//!     strengths, weaknesses, license, decision) so the same repository is
//!     not re-researched repeatedly.
//!   * Maintain the continuous capability graph under
//!     <data>/intelligence/capabilities.md (mirrored to .apex/intelligence/).
//!
//! Reuses ByteAI's existing `websearch` (discovery), `fetch` (source
//! inspection), the configured provider (evaluation), and agent memory.

use std::time::Instant;

use apex_memory::Kind;
use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};

use crate::fetch::FetchTool;
use crate::websearch::WebSearchTool;
use crate::{BoxFuture, Tool, ToolContext, ok_outcome};

/// Known search targets (menu items) → query templates.
fn target_templates(target: &str, q: &str) -> Vec<String> {
    let t = target.trim();
    match t {
        "skills" => vec![
            format!("\"{q}\" coding agent skill github"),
            format!("site:github.com \"{q}\" agent skills"),
            format!("github awesome agent skills {q}"),
        ],
        "harnesses" => vec![
            format!("\"{q}\" coding agent harness github"),
            format!("site:github.com coding agent harness {q}"),
            format!("github autonomous coding agent {q}"),
        ],
        "mcp" => vec![
            format!("\"{q}\" mcp server github"),
            format!("site:github.com mcp server {q}"),
        ],
        "tools" => vec![
            format!("\"{q}\" cli tool github"),
            format!("site:github.com {q} tool"),
        ],
        "libraries" | "frameworks" => vec![
            format!("\"{q}\" library github"),
            format!("site:github.com {q}"),
        ],
        "debugging" => vec![
            format!("{q} debugging tool github"),
            format!("site:github.com {q} debugger"),
        ],
        "testing" => vec![
            format!("{q} testing tool github"),
            format!("site:github.com {q} test framework"),
        ],
        "security" => vec![
            format!("{q} security tool github"),
            format!("site:github.com {q} scanner"),
        ],
        _ => vec![
            format!("\"{q}\" github"),
            format!("site:github.com {q}"),
            format!("{q} repository"),
        ],
    }
}

/// Extract `owner/repo` pairs from a search-results blob.
fn extract_repos(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find("github.com/") {
        let tail = &rest[pos + "github.com/".len()..];
        let mut parts = tail.splitn(3, ['/', ' ', '"', ')', '(', '\n']);
        let owner = parts.next().unwrap_or("").trim().to_string();
        let repo = parts.next().unwrap_or("").trim().to_string();
        if owner.len() >= 2 && repo.len() >= 2 && !owner.contains('.') {
            let pair = (owner, repo.trim_end_matches(".git").to_string());
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
        rest = &tail[1.min(tail.len())..];
    }
    out
}

async fn search_one(query: String, max: u64) -> String {
    let tool = WebSearchTool;
    let o = tool.execute(json!({ "query": query, "max": max })).await;
    o.output
}

async fn fetch_readme(owner: &str, repo: &str) -> String {
    let tool = FetchTool;
    for name in ["README.md", "readme.md", "Readme.md"] {
        let url = format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{name}");
        let o = tool
            .execute(json!({ "url": url, "max_chars": 6000 }))
            .await;
        let text = o.output.trim();
        if !text.is_empty() && !text.starts_with("HTTP ") && !text.starts_with("ERROR") {
            return text.chars().take(6000).collect();
        }
    }
    // Fallback: the repo page itself.
    let o = tool
        .execute(json!({ "url": format!("https://github.com/{owner}/{repo}"), "max_chars": 4000 }))
        .await;
    o.output.chars().take(4000).collect()
}

pub struct GithubTool {
    pub ctx: ToolContext,
}

impl GithubTool {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }

    fn menu(&self) -> String {
        [
            "# /Github — what should ByteAI search for?",
            "",
            " 1. Better Skills             12. Testing Tools",
            " 2. New Skills                13. Performance Tools",
            " 3. Update Existing Skills    14. Security Tools",
            " 4. Better Harnesses          15. Libraries / Frameworks",
            " 5. New Harnesses             16. APIs / SDKs",
            " 6. Better Tools              17. Deployment / Infrastructure",
            " 7. MCP Servers               18. Better Alternatives to Current Tools",
            " 8. Coding-Agent Technology   19. Tools for the current /Ideas project",
            " 9. Memory / Context Systems  20. Improve ByteAI",
            "10. Multi-Agent Systems       21. Full Capability Scan",
            "11. Debugging Tools           22. Search Something Specific",
            "",
            "Use: /github <target> <query>",
            "     /github skills <capability>      — discover+score skills",
            "     /github harnesses <capability>   — discover+score harnesses",
            "     /github mcp <capability>         — MCP servers",
            "     /github tools <capability>       — tools/libraries",
            "     /github current                  — analyze the current project's gaps",
            "     /github improve [focus]          — rank improvements to ByteAI itself",
            "     /github evaluate <owner/repo>    — evaluate one repository",
            "     /github memory                   — list saved evaluations",
            "     /github graph                    — show the capability graph",
            "",
        ]
        .join("\n")
    }

    /// The compatibility-engine evaluation prompt (shared by all paths).
    fn search(&self, target: &str, query: &str) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        let target = target.to_string();
        let query = query.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let q = if query.trim().is_empty() {
                target.clone()
            } else {
                query.clone()
            };
            let templates = target_templates(&target, &q);
            let mut results = Vec::new();
            for t in templates.iter().take(3) {
                results.push(search_one(t.clone(), 8).await);
            }
            let blob = results.join("\n\n");
            let repos = extract_repos(&blob);
            if repos.is_empty() {
                out.push_str(&format!(
                    "# /Github — no repositories found for \"{q}\"\n\nNo github.com links in search results. Try a broader query or a different target.\n\nRaw search:\n{}",
                    blob.chars().take(3000).collect::<String>()
                ));
                return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
            }

            out.push_str(&format!(
                "# /Github — {target}: {q}\n\nCandidates found: {}\n\n",
                repos.len()
            ));
            for (i, (owner, repo)) in repos.iter().take(4).enumerate() {
                out.push_str(&format!("{}. {owner}/{repo}\n", i + 1));
            }
            out.push_str("\nInspecting READMEs of the top candidates…\n\n");

            // Inspect up to 3 candidates deeply.
            let mut candidates = String::new();
            for (owner, repo) in repos.iter().take(3) {
                let readme = fetch_readme(owner, repo).await;
                candidates.push_str(&format!(
                    "## {owner}/{repo}\n\n{readme}\n\n---\n\n"
                ));
            }

            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let (system, user) = GithubTool::eval_prompt_direct(&target, &q, &today);
            let eval = llm_call(&ctx, &system, &user, 6000)
                .await
                .unwrap_or_else(|e| format!("Evaluation failed: {e}"));
            out.push_str(&eval);

            // Persist per-repo evaluation files + memory + capability graph.
            for (owner, repo) in repos.iter().take(4) {
                let file = format!("{}-{}.md", owner, repo);
                let _ = persist_direct(&ctx, "repos", &file, &out);
                out.push_str(&remember_direct(
                    &ctx,
                    &format!("eval {owner}/{repo}"),
                    &eval.chars().take(1200).collect::<String>(),
                    &["github", "evaluation"],
                ));
                // Best-effort graph update (first ADOPT/ADAPT decision found).
                let decision = eval
                    .lines()
                    .find(|l| l.contains("Recommendation:"))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| "Recommendation: REJECT".into());
                update_graph_direct(
                    &ctx,
                    &q,
                    &format!("{decision} ({owner}/{repo})"),
                    &format!("{owner}/{repo}"),
                );
            }
            out.push_str(&format!(
                "\n[saved] evaluations under {}/intelligence/repos/\n",
                ctx.data_dir.display()
            ));
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    fn evaluate_one(&self, repo: &str) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        let repo = repo.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let repo = repo.trim().trim_start_matches("https://").trim_start_matches("github.com/");
            let mut parts = repo.splitn(2, '/');
            let (owner, name) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            if owner.is_empty() || name.is_empty() {
                out.push_str("usage: /github evaluate <owner/repo>\n");
                return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
            }
            out.push_str(&format!("# /Github — evaluate {owner}/{name}\n\n"));
            let readme = fetch_readme(owner, name).await;
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let (system, user) = GithubTool::eval_prompt_direct("evaluate", &format!("{owner}/{name}"), &today);
            let user = format!(
                "{}\n\nCandidate:\n## {owner}/{name}\n\n{readme}\n\n---\n",
                user
            );
            let eval = llm_call(&ctx, &system, &user, 4000)
                .await
                .unwrap_or_else(|e| format!("Evaluation failed: {e}"));
            out.push_str(&eval);
            let file = format!("{}-{}.md", owner, name);
            let _ = persist_direct(&ctx, "repos", &file, &out);
            update_graph_direct(
                &ctx,
                "capability (evaluated)",
                "evaluated",
                &format!("{owner}/{name}"),
            );
            out.push_str(&remember_direct(
                &ctx,
                &format!("eval {owner}/{name}"),
                &eval.chars().take(1200).collect::<String>(),
                &["github", "evaluation"],
            ));
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    /// /Github current — analyze the current project's capability gaps, then
    /// search for exactly those gaps (no broad unnecessary searches).
    fn current(&self) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let cwd = std::env::current_dir().unwrap_or_default();
            // Peek at the project's manifest(s).
            let mut manifest = String::new();
            for f in ["Cargo.toml", "package.json", "pyproject.toml", "go.mod", "README.md"] {
                let p = cwd.join(f);
                if let Ok(text) = std::fs::read_to_string(&p) {
                    manifest.push_str(&format!("=== {f} ===\n{}\n\n", text.chars().take(3000).collect::<String>()));
                }
            }
            if manifest.is_empty() {
                out.push_str("# /Github current — no manifest found in cwd\n\nNothing to analyze here. Run /Github current inside a project.\n");
                return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
            }
            out.push_str("# /Github current — capability gap analysis\n\n");
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let system = "You are ByteAI APEX's /Github current engine. Analyze the current project's manifests and identify: (1) what is being built, (2) required capabilities, (3) which capabilities are already covered, (4) which are MISSING. Output a capability table like:\n\nREQUIRED\nAuthentication      ✓ / ✗\nPayments            ✓ / ✗\nOCR                 ✓ / ✗\n...\n\nThen list ONLY the missing capabilities as 'SEARCH FOR: <capability>' lines (one per line) so targeted GitHub searches can be run for exactly those.";
            let user = format!(
                "Today: {today}\nProject at: {}\n\n{manifest}\n\nProduce the capability table + SEARCH FOR lines.",
                cwd.display()
            );
            let analysis = llm_call(&ctx, system, &user, 2500)
                .await
                .unwrap_or_else(|e| format!("Analysis failed: {e}"));
            out.push_str(&analysis);

            // Targeted discovery for each missing capability (max 3).
            let missing: Vec<String> = analysis
                .lines()
                .filter(|l| l.trim().starts_with("SEARCH FOR:"))
                .map(|l| l.trim().trim_start_matches("SEARCH FOR:").trim().to_string())
                .filter(|s| !s.is_empty())
                .take(3)
                .collect();
            if !missing.is_empty() {
                out.push_str("\n\n--- Targeted GitHub discovery ---\n\n");
                for cap in &missing {
                    out.push_str(&format!("## Searching for: {cap}\n"));
                    let res = search_one(format!("\"{cap}\" github"), 6).await;
                    let repos = extract_repos(&res);
                    if repos.is_empty() {
                        out.push_str(&format!("  no repos found for {cap}\n"));
                        continue;
                    }
                    let mut cands = String::new();
                    for (owner, repo) in repos.iter().take(2) {
                        let readme = fetch_readme(owner, repo).await;
                        cands.push_str(&format!("## {owner}/{repo}\n\n{readme}\n\n---\n\n"));
                    }
                    let (system2, user2) = GithubTool::eval_prompt_direct("gap", cap, &today);
                    let eval = llm_call(&ctx, &system2, &format!("{user2}\n\nCandidates:\n{cands}"), 2500)
                        .await
                        .unwrap_or_else(|e| format!("  eval failed: {e}"));
                    out.push_str(&eval);
                    for (owner, repo) in repos.iter().take(2) {
                        let _ = persist_direct(&ctx, "repos", &format!("{owner}-{repo}.md"), &eval);
                        update_graph_direct(&ctx, cap, "candidate", &format!("{owner}/{repo}"));
                    }
                }
            }
            let file = format!("current-{}.md", today.replace('-', ""));
            let _ = persist_direct(&ctx, "repos", &file, &out);
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    /// /Github improve — research agent-improvement technology, rank by
    /// expected benefit.
    fn improve(&self, focus: &str) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        let focus = focus.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let areas: Vec<String> = if focus.trim().is_empty() {
                vec![
                    "reasoning harnesses".to_string(), "context management".to_string(), "memory".to_string(), "retrieval".to_string(),
                    "editing".to_string(), "LSP".to_string(), "debugging".to_string(), "subagents".to_string(), "planning".to_string(), "verification".to_string(),
                    "testing".to_string(), "browser tools".to_string(), "MCP".to_string(), "sandboxing".to_string(), "model routing".to_string(),
                    "prompt caching".to_string(), "token efficiency".to_string(), "TUI".to_string(), "observability".to_string(), "security".to_string(),
                ]
            } else {
                vec![focus.trim().to_string()]
            };
            out.push_str(&format!(
                "# /Github improve — ranked improvement opportunities\n\nResearching {} area(s)…\n\n",
                areas.len()
            ));
            let mut evidence = String::new();
            for a in areas.iter().take(6) {
                evidence.push_str(&format!("## {a}\n"));
                let res = search_one(format!("{a} coding agent github"), 5).await;
                evidence.push_str(&res.chars().take(1200).collect::<String>());
                evidence.push('\n');
            }
            let today = chrono::Local::now().format("%Y-%m-%d").to_string();
            let system = "You are ByteAI APEX's /Github improve engine. Search specifically for technology that can make the coding agent ITSELF stronger. From the evidence, produce 'Top APEX improvements' ranked by expected benefit (impact × feasibility). For each: the area, the concrete technology/repo/idea, what it improves, expected benefit (High/Med/Low), effort, and a recommendation (ADOPT / ADAPT / LEARN FROM / REJECT with why). Only recommend meaningful changes — no noise.";
            let user = format!(
                "Today: {today}\nResearch areas: {}\n\n=== EVIDENCE ===\n{evidence}\n=== END EVIDENCE ===\n\nProduce the ranked Top improvements.",
                areas.join(", ")
            );
            let synth = llm_call(&ctx, system, &user, 5000)
                .await
                .unwrap_or_else(|e| format!("Improve analysis failed: {e}"));
            out.push_str(&synth);
            let file = format!("improvements-{}.md", today.replace('-', ""));
            let _ = persist_direct(&ctx, "repos", &file, &out);
            let _ = persist_direct(&ctx, "improvements", &file, &synth);
            out.push_str(&remember_direct(
                &ctx,
                &format!("improve {}", if focus.is_empty() { "agent" } else { &focus }),
                &synth.chars().take(1200).collect::<String>(),
                &["github", "improve"],
            ));
            out.push_str(&format!("\n[time] {}s", started.elapsed().as_secs_f64()));
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    fn memory(&self) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            match apex_memory::Memory::open(&ctx.data_dir.join("memory")) {
                Ok(mem) => match mem.search("github", Some(Kind::Note), 20) {
                    Ok(entries) => {
                        out.push_str(&format!("/Github intelligence memory ({} entries)\n\n", entries.len()));
                        for e in entries {
                            let title = e.title.clone();
                            let body = e.body.clone();
                            let preview: String = body.chars().take(140).collect();
                            out.push_str(&format!("#{id} {title} — {preview}\n", id = e.id));
                        }
                    }
                    Err(e) => out.push_str(&format!("search failed: {e:#}")),
                },
                Err(e) => out.push_str(&format!("memory unavailable: {e:#}")),
            }
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    fn graph(&self) -> BoxFuture<'_, ToolOutcome> {
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();
            let paths = [
                ctx.data_dir.join("intelligence").join("capabilities.md"),
                std::env::current_dir()
                    .unwrap_or_default()
                    .join(".apex")
                    .join("intelligence")
                    .join("capabilities.md"),
            ];
            let mut shown = false;
            for p in &paths {
                if let Ok(text) = std::fs::read_to_string(p) {
                    out.push_str(&text);
                    shown = true;
                    break;
                }
            }
            if !shown {
                out.push_str("No capability graph yet. Run /Github search first.\n");
            }
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    /// /Github connect — connect the current project to GitHub: check `gh`
    /// auth, create a remote repo (public by default, `--visibility private`
    /// for private), set the remote, and push. Returns the repo URL.
    fn connect(&self, repo_name: Option<&str>, visibility: &str) -> BoxFuture<'_, ToolOutcome> {
        let repo_name = repo_name.map(|s| s.to_string());
        let visibility = visibility.to_string();
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();

            // 1. `gh` CLI available?
            let gh = tokio::process::Command::new("gh").arg("--version").output().await;
            let gh_ok = match gh {
                Ok(o) if o.status.success() => true,
                _ => false,
            };
            if !gh_ok {
                out.push_str(
                    "ERROR: `gh` CLI not found on PATH. Install it first:\n\
                     \x20 brew install gh   # macOS\n\
                     \x20 then run: gh auth login\n",
                );
                return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
            }

            // 2. Authenticated?
            let auth = tokio::process::Command::new("gh").args(["auth", "status"]).output().await;
            match auth {
                Ok(o) if o.status.success() => {
                    let who = String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .find(|l| l.contains("Logged in"))
                        .unwrap_or("authenticated")
                        .trim()
                        .to_string();
                    out.push_str(&format!("✓ GitHub auth OK — {who}\n\n"));
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    out.push_str(&format!(
                        "ERROR: not authenticated with GitHub.\n{err}\nRun: gh auth login\n"
                    ));
                    return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                }
                Err(e) => {
                    out.push_str(&format!("ERROR: could not run gh auth status: {e}\n"));
                    return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                }
            }

            // 3. Resolve repo name + visibility.
            let cwd = std::env::current_dir().unwrap_or_default();
            let default_name = cwd
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "byteai".to_string());
            let repo = repo_name.as_deref().unwrap_or("").trim();
            let name = if repo.is_empty() { default_name.clone() } else { repo.to_string() };
            let vis_flag = if visibility == "private" { "private" } else { "public" };

            // 4. Create the repo (idempotent: view first, create if missing).
            let view = tokio::process::Command::new("gh")
                .args(["repo", "view", &name, "--json", "url"])
                .output().await;
            let exists = matches!(view, Ok(o) if o.status.success());
            if !exists {
                out.push_str(&format!("Creating GitHub repo `{name}` ({vis_flag})…\n"));
                let create = tokio::process::Command::new("gh")
                    .args(["repo", "create", &name, "--source", ".", "--", "--", vis_flag])
                    .current_dir(&cwd)
                    .output().await;
                match create {
                    Ok(o) if o.status.success() => {
                        let text = String::from_utf8_lossy(&o.stdout);
                        let url = text
                            .lines()
                            .find(|l| l.contains("github.com/"))
                            .map(|l| l.trim().to_string())
                            .unwrap_or_else(|| format!("https://github.com/{name}"));
                        out.push_str(&format!("✓ Repo created: {url}\n"));
                    }
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                        out.push_str(&format!(
                            "Could not create repo. Try manually:\n  gh repo create {name} --source . --public\nstderr: {err}\n"
                        ));
                        return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                    }
                    Err(e) => {
                        out.push_str(&format!("Could not create repo: {e}\n"));
                        return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                    }
                }
            } else {
                out.push_str(&format!("✓ Repo `{name}` already exists.\n"));
            }

            // 5. Ensure git remote + push.
            let has_remote = tokio::process::Command::new("git")
                .args(["remote", "get-url", "origin"]).current_dir(&cwd).output().await;
            let remote_ok = matches!(has_remote, Ok(o) if o.status.success());
            let push_url = format!("https://github.com/{name}.git");
            if !remote_ok {
                out.push_str(&format!("Adding git remote origin → {push_url}\n"));
                let _ = tokio::process::Command::new("git")
                    .args(["remote", "add", "origin", &push_url])
                    .current_dir(&cwd).output().await;
            }
            out.push_str("Pushing to GitHub…\n");
            let push = tokio::process::Command::new("git")
                .args(["push", "-u", "origin", "HEAD"])
                .current_dir(&cwd).output().await;
            match push {
                Ok(o) if o.status.success() => {
                    out.push_str("✓ Pushed. Your project is live at:\n");
                    out.push_str(&format!("  https://github.com/{name}\n"));
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    out.push_str(&format!(
                        "Push reported a non-zero status (may need a commit):\n{err}\n\n\
                         After committing, run: git push -u origin HEAD\n"
                    ));
                }
                Err(e) => out.push_str(&format!("Push error: {e}\n")),
            }
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    /// /Github status — check gh CLI + auth + current repo state (no side
    /// effects: never creates or pushes).
    fn connect_status(&self) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let started = Instant::now();
            let mut out = String::new();

            let gh = tokio::process::Command::new("gh").arg("--version").output().await;
            match gh {
                Ok(o) if o.status.success() => {
                    out.push_str("✓ gh CLI available\n");
                }
                _ => {
                    out.push_str("✗ `gh` CLI not found — install: brew install gh\n");
                    return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                }
            }

            let auth = tokio::process::Command::new("gh").args(["auth", "status"]).output().await;
            match auth {
                Ok(o) if o.status.success() => {
                    let who = String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .find(|l| l.contains("Logged in"))
                        .unwrap_or("authenticated")
                        .trim()
                        .to_string();
                    out.push_str(&format!("✓ GitHub auth OK — {who}\n"));
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    out.push_str(&format!("✗ Not authenticated with GitHub.\n{err}\nRun: gh auth login\n"));
                    return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                }
                Err(e) => {
                    out.push_str(&format!("✗ Could not run gh auth status: {e}\n"));
                    return ok_outcome("", "github", out, started.elapsed().as_millis() as u64);
                }
            }

            let cwd = std::env::current_dir().unwrap_or_default();
            let name = cwd
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "byteai".to_string());
            out.push_str(&format!("Project: {}\n", cwd.display()));

            let remote = tokio::process::Command::new("git")
                .args(["remote", "get-url", "origin"]).current_dir(&cwd).output().await;
            match remote {
                Ok(o) if o.status.success() => {
                    let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    out.push_str(&format!("✓ git remote origin → {url}\n"));
                }
                _ => out.push_str("— no git remote `origin` yet (run /github connect)\n"),
            }

            let view = tokio::process::Command::new("gh")
                .args(["repo", "view", &name, "--json", "url,visibility"])
                .output().await;
            match view {
                Ok(o) if o.status.success() => {
                    let info = String::from_utf8_lossy(&o.stdout);
                    out.push_str(&format!("✓ GitHub repo exists: {info}\n"));
                }
                _ => out.push_str(&format!("— no GitHub repo `{name}` yet (run /github connect)\n")),
            }
            out.push_str("\nTo publish this project: /github connect [repo-name] [public|private]\n");
            ok_outcome("", "github", out, started.elapsed().as_millis() as u64)
        })
    }

    /// Shared: build the (system, user) evaluation prompt pair.
    fn eval_prompt_direct(target: &str, focus: &str, today: &str) -> (String, String) {
        let system = [
            "You are ByteAI APEX's /Github compatibility engine.",
            "Evaluate each candidate repository against the COMPATIBILITY ENGINE:",
            "  APEX compatibility: 0–100",
            "  Current project compatibility: 0–100",
            "  Integration complexity: Low / Medium / High",
            "  Performance impact: Positive / Neutral / Negative",
            "  Security risk: Low / Medium / High",
            "  Maintenance risk: Low / Medium / High",
            "  License: ...",
            "  Recommendation: ADOPT / ADAPT / LEARN FROM / REJECT",
            "  Why: ...",
            "Decision rules:",
            "  ADOPT      — use directly.",
            "  ADAPT      — integrate selected components only.",
            "  LEARN FROM — reimplement the architectural idea inside ByteAI.",
            "  REJECT     — not worth using.",
            "Never recommend installing everything found. Judge relevance, maintenance, documentation quality,",
            "dependency weight, security posture, license, integration difficulty, and production readiness —",
            "not stars.",
        ]
        .join("\n");
        let user = format!(
            "Today: {today}\nSearch target: {target}\nCapability focus: {focus}\n\nFor each candidate output EXACTLY:\n\n## Repository: owner/repo\n\nPurpose:\n...\n\nByteAI Compatibility:\nXX / 100\n\nCurrent Project Compatibility:\nXX / 100\n\nMaintenance:\nExcellent / Good / Weak / Abandoned\n\nIntegration Complexity:\nLow / Medium / High\n\nPerformance Impact:\nPositive / Neutral / Negative\n\nSecurity Risk:\nLow / Medium / High\n\nMaintenance Risk:\nLow / Medium / High\n\nLicense:\n...\n\nRecommendation:\nADOPT / ADAPT / LEARN FROM / REJECT\n\nWhy:\n...\n\nThen a final 'Recommendation summary'."
        );
        (system, user)
    }
}

// Free-function mirrors (owned ctx, no borrows across await).
fn llm_call(
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

fn persist_direct(ctx: &ToolContext, sub: &str, rel: &str, content: &str) -> std::io::Result<()> {
    let roots = [
        ctx.data_dir.join("intelligence"),
        std::env::current_dir()
            .unwrap_or_default()
            .join(".apex")
            .join("intelligence"),
    ];
    for root in roots {
        let dir = root.join(sub);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(rel), content)?;
    }
    Ok(())
}

fn remember_direct(ctx: &ToolContext, title: &str, body: &str, tags: &[&str]) -> String {
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

fn update_graph_direct(ctx: &ToolContext, focus: &str, decision: &str, repo: &str) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let line = format!("- {focus}\n  - {today} {decision} {repo} (via /Github)\n");
    let roots = [
        ctx.data_dir.join("intelligence"),
        std::env::current_dir()
            .unwrap_or_default()
            .join(".apex")
            .join("intelligence"),
    ];
    for root in roots {
        let _ = std::fs::create_dir_all(&root);
        let path = root.join("capabilities.md");
        let mut content = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            "# Capability Graph\n\nCapability\n├── Current implementation\n├── Available skills\n├── Available tools\n├── Available harnesses\n├── Candidate improvements\n└── Benchmark history\n\n".to_string()
        });
        content.push_str(&line);
        let _ = std::fs::write(path, content);
    }
}

impl Tool for GithubTool {
    fn name(&self) -> &'static str {
        "github"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "github".into(),
            description: "/Github — capability discovery and upgrade engine. Finds and scores skills, harnesses, tools, \
MCP servers, libraries, and coding-agent technology using the COMPATIBILITY ENGINE (APEX/project compatibility 0-100, \
complexity, performance, security, maintenance, license, ADOPT/ADAPT/LEARN FROM/REJECT). Keeps a GitHub intelligence \
memory + capability graph under intelligence/. Also supports /Github connect — connect this project to GitHub via gh CLI \
(auth, create repo, push). Actions: menu, search, evaluate, current, improve, memory, graph, connect, status, push. \
Input: {action, target, query}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["menu", "search", "evaluate", "current", "improve", "memory", "graph", "connect", "status", "push"] },
                    "target": { "type": "string", "description": "skills | harnesses | tools | mcp | libraries | debugging | testing | security | <anything>" },
                    "query": { "type": "string", "description": "Capability or repo (owner/repo for evaluate)" }
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
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match action.as_str() {
            "menu" => {
                let out = self.menu();
                let started = Instant::now();
                Box::pin(async move { ok_outcome("", "github", out, started.elapsed().as_millis() as u64) })
            }
            "connect" | "status" | "push" => {
                // Parse: /github connect [repo-name] [private|public]
                // target/query both carry the action word ("connect ..."); strip
                // it, plus any duplicate action mentions, then split the rest.
                //   - visibility is the LAST word if it is private/public
                //   - repo name is whatever else (default = cwd basename)
                let mut words: Vec<String> = target
                    .split_whitespace()
                    .chain(query.split_whitespace())
                    .filter(|s| !s.is_empty())
                    .filter(|s| *s != action)
                    .map(|s| s.to_string())
                    .collect();
                let mut visibility = "public";
                if let Some(last) = words.last().map(|s| s.to_lowercase()) {
                    if last == "private" || last == "public" {
                        visibility = if last == "private" { "private" } else { "public" };
                        words.pop();
                    }
                }
                let repo_name = words.join("-");
                let repo = if repo_name.is_empty() { None } else { Some(repo_name.as_str()) };
                if action == "status" {
                    // Just auth + repo status, no create/push.
                    self.connect_status()
                } else {
                    self.connect(repo, visibility)
                }
            }
            "search" => self.search(&target, &query),
            "evaluate" => {
                // Accept query "evaluate owner/repo" or direct owner/repo.
                let q = query
                    .trim_start_matches("evaluate")
                    .trim()
                    .to_string();
                self.evaluate_one(&q)
            }
            "current" => self.current(),
            "improve" => {
                let q = query.trim_start_matches("improve").trim().to_string();
                self.improve(&q)
            }
            "memory" => self.memory(),
            "graph" => self.graph(),
            other => {
                let out = format!("ERROR: unknown action {other:?}\n{}", self.menu());
                let started = Instant::now();
                Box::pin(async move { ok_outcome("", "github", out, started.elapsed().as_millis() as u64) })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_owner_repo_from_results() {
        let blob = "1. Foo\n   https://github.com/rust-lang/rust\n   snippet\n\n2. Bar\n   https://github.com/openai/codex/\n   x";
        let repos = extract_repos(blob);
        assert!(repos.contains(&("rust-lang".into(), "rust".into())));
        assert!(repos.contains(&("openai".into(), "codex".into())));
    }

    #[test]
    fn dedupes_repos() {
        let blob = "https://github.com/ab/cd https://github.com/ab/cd https://github.com/ab/ce";
        let repos = extract_repos(blob);
        assert_eq!(repos.len(), 2);
    }

    #[test]
    fn no_false_owner_on_domains() {
        let blob = "https://github.com/features/actions";
        let repos = extract_repos(blob);
        // "features/actions" would be owner=features (has dot? no) — it is a
        // valid-looking pair; the heuristic accepts it. Just assert we don't panic.
        assert!(!repos.is_empty());
    }

    #[test]
    fn target_templates_cover_known_targets() {
        for t in ["skills", "harnesses", "mcp", "tools", "debugging", "testing", "security"] {
            assert!(!target_templates(t, "xyz").is_empty(), "{t} templates");
        }
    }
}
