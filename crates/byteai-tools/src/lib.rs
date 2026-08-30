//! Native tool implementations for the ByteAi agent.
//!
//! Phase 1 tools: shell, read, search (literal + regex), edit (exact match),
//! todo, note (minimal Layer-B memory seed).
//! Phase 2: lsp (diagnostics, symbols, hover, def, refs, rename, format),
//! read extended (symbols, function, imports via byteai-ast),
//! search extended (symbol mode via byteai-ast),
//! edit extended (contextual + whole-file + LSP validation).

pub mod edit;
pub mod lsp;
mod verify;
mod debug;
mod memory;
pub mod skills;
mod spawn;
mod review;
mod plugin;
mod fetch;
mod websearch;
mod graph;
mod plan;
mod route;
mod council;
mod govern;
mod git;
mod sandbox;
mod crew;
mod mcp;
pub mod schedule;
pub mod cron;
mod proc;
mod sessionsearch;
pub mod herdr;
mod pi;
mod workflow;
mod improve;
mod gates;
mod ideas;
pub mod github;
pub mod note;
pub mod read;
pub mod search;
pub mod shell;
pub mod todo;
mod backup;
mod worktree;
mod secrets;
mod kanban;
mod memsearch;
mod moa;
mod notify;
mod dan;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use byteai_lsp::LspRegistry;
use byteai_types::{ToolDef, ToolOutcome};
use serde_json::Value;

/// A boxed async execute future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A native tool: schema definition + async execution.
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn def(&self) -> ToolDef;
    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome>;

    /// Long-running tools (delegation/spawn, review swarms…) legitimately
    /// take far longer than a normal tool call. The agent loop gives them a
    /// dedicated, much larger timeout instead of the generic per-tool cap,
    /// so a real multi-agent delegation is never killed at 300s mid-flight.
    fn long_running(&self) -> bool {
        false
    }
}

/// Shared context passed to tools at construction time.
#[derive(Clone)]
pub struct ToolContext {
    pub data_dir: PathBuf,
    pub lsp: Option<Arc<LspRegistry>>,
    pub dap: Option<Arc<byteai_dap::DapRegistry>>,
    /// Provider client for model-routing / council / governance tools
    /// (None in offline or tool-only contexts).
    pub client: Option<byteai_provider::Client>,
    /// Effective default model (used by route/council fallbacks).
    pub default_model: String,
    /// Per-subagent iteration budget for the spawn/delegation tool.
    pub delegation_max_iterations: Option<u32>,
    /// Wall-clock budget (seconds) forwarded to spawned subagents.
    pub run_budget_seconds: Option<u64>,
}

impl ToolContext {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir, lsp: None, dap: None, client: None, default_model: String::new(), delegation_max_iterations: None, run_budget_seconds: None }
    }
    pub fn with_lsp(data_dir: PathBuf, lsp: Arc<LspRegistry>) -> Self {
        Self { data_dir, lsp: Some(lsp), dap: None, client: None, default_model: String::new(), delegation_max_iterations: None, run_budget_seconds: None }
    }
    pub fn with_all(data_dir: PathBuf, lsp: Arc<LspRegistry>, dap: Arc<byteai_dap::DapRegistry>) -> Self {
        Self { data_dir, lsp: Some(lsp), dap: Some(dap), client: None, default_model: String::new(), delegation_max_iterations: None, run_budget_seconds: None }
    }
    pub fn with_provider(mut self, client: byteai_provider::Client, default_model: String) -> Self {
        self.client = Some(client);
        self.default_model = default_model;
        self
    }
    /// Attach interaction-budget limits (used by the spawn tool so delegated
    /// subagents get their own independent iteration/wall-clock budgets).
    pub fn with_limits(mut self, delegation_max_iterations: Option<u32>, run_budget_seconds: Option<u64>) -> Self {
        self.delegation_max_iterations = delegation_max_iterations;
        self.run_budget_seconds = run_budget_seconds;
        self
    }
}

/// Tool registry: name -> tool.
#[derive(Default)]
pub struct Registry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl Registry {
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.def()).collect()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    /// All built-in tools for Phase 1 + Phase 2.
    pub fn builtins(ctx: &ToolContext) -> Self {
        let mut r = Registry::default();
        r.register(Arc::new(shell::ShellTool));
        r.register(Arc::new(read::ReadTool));
        r.register(Arc::new(search::SearchTool));
        r.register(Arc::new(edit::EditTool::new(ctx.lsp.clone())));
        r.register(Arc::new(todo::TodoTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(note::NoteTool::new(ctx.data_dir.join("memory").join("notes"))));
        r.register(Arc::new(lsp::LspTool::new(ctx.lsp.clone())));
        r.register(Arc::new(verify::VerifyTool::new(ctx.lsp.clone())));
        r.register(Arc::new(debug::DebugTool::new(ctx.dap.clone())));
        r.register(Arc::new(memory::MemoryTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(skills::SkillsTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(spawn::SpawnTool::new(ctx.clone())));
        r.register(Arc::new(review::ReviewTool::new(ctx.lsp.clone())));
        r.register(Arc::new(plugin::PluginTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(fetch::FetchTool));
        r.register(Arc::new(websearch::WebSearchTool));
        r.register(Arc::new(graph::GraphTool));
        r.register(Arc::new(plan::PlanTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(route::RouteTool::new(ctx.clone())));
        r.register(Arc::new(council::CouncilTool::new(ctx.clone())));
        r.register(Arc::new(govern::GovernTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(git::GitTool));
        r.register(Arc::new(sandbox::SandboxTool));
        r.register(Arc::new(crew::CrewTool::new(ctx.clone())));
        r.register(Arc::new(mcp::McpTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(schedule::ScheduleTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(workflow::WorkflowTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(improve::ImproveTool::new(ctx.clone())));
        r.register(Arc::new(gates::GatesTool));
        r.register(Arc::new(ideas::IdeasTool::new(ctx.clone())));
        r.register(Arc::new(github::GithubTool::new(ctx.clone())));
        r.register(Arc::new(backup::BackupTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(worktree::WorktreeTool));
        r.register(Arc::new(secrets::SecretsTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(kanban::KanbanTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(memsearch::MemsearchTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(moa::MoaTool::new(
            Arc::new(ctx.client.clone()),
            Arc::new(ctx.default_model.clone()),
        )));
        r.register(Arc::new(notify::NotifyTool));
        r.register(Arc::new(proc::ProcTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(sessionsearch::SessionSearchTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(pi::PiTool::new(ctx.data_dir.clone())));
        r.register(Arc::new(dan::DanMethodologyTool));
        r
    }
}

/// Build a `ToolOutcome` from a result string.
pub fn ok_outcome(call_id: &str, name: &str, output: impl Into<String>, elapsed_ms: u64) -> ToolOutcome {
    ToolOutcome { call_id: call_id.to_string(), name: name.to_string(), output: output.into(), ok: true, elapsed_ms }
}

/// Build a failed `ToolOutcome`.
pub fn err_outcome(call_id: &str, name: &str, err: &anyhow::Error, elapsed_ms: u64) -> ToolOutcome {
    ToolOutcome {
        call_id: call_id.to_string(),
        name: name.to_string(),
        output: format!("ERROR: {err:#}"),
        ok: false,
        elapsed_ms,
    }
}