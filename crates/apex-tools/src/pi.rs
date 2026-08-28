//! `pi` — Pi agent package compatibility layer.
//!
//! Installs Pi agent packages (npm/git/local) and makes their resources
//! available to ByteAi:
//!   - skills/  → `<data_dir>/skills/` (SKILL.md — native ByteAi format)
//!   - prompts/ → `<data_dir>/prompts/`
//!   - extensions/ → generates a Node bridge shim + registers proxy tools
//!
//! Usage from the agent:
//!   pi install npm:pi-hermes-memory     — install from npm registry
//!   pi install git:github.com/user/repo — install from git
//!   pi install /path/to/package        — install from local path
//!   pi list                             — show installed packages
//!   pi remove <name>                    — uninstall a package
//!   pi reload                           — regenerate bridges for all

use std::path::{Path, PathBuf};

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{BoxFuture, Tool, ok_outcome};

/// Manifest for installed pi packages.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct PiManifest {
    packages: Vec<PiPackage>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PiPackage {
    name: String,
    spec: String,
    version: String,
    /// Skills extracted into `<data_dir>/skills/`.
    skills_installed: usize,
    /// Prompts extracted into `<data_dir>/prompts/`.
    prompts_installed: usize,
    /// Extensions found (may need the Node bridge).
    extensions_found: Vec<String>,
}

pub struct PiTool {
    data_dir: PathBuf,
}

impl PiTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

impl Tool for PiTool {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "pi".into(),
            description: "Pi agent package compatibility. Actions: install {spec}, list, remove {name}, reload. "
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["install", "list", "remove", "reload"],
                        "description": "Action to perform"
                    },
                    "spec": {
                        "type": "string",
                        "description": "Package spec (npm:name, git:url, or path) for install"
                    },
                    "name": {
                        "type": "string",
                        "description": "Package name for remove"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let data_dir = self.data_dir.clone();
        Box::pin(async move {
            let started = std::time::Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("").to_string();

            match action.as_str() {
                "install" => {
                    let spec = args.get("spec").and_then(Value::as_str).unwrap_or("");
                    if spec.is_empty() {
                        return ok_outcome("", "pi", "ERROR: `spec` required (npm:name, git:url, or path)", started.elapsed().as_millis() as u64);
                    }
                    install_pi_package(&data_dir, spec).await
                }
                "list" => {
                    let m = {
                        let p = data_dir.join("pi").join("manifest.json");
                        std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<PiManifest>(&s).ok()).unwrap_or_default()
                    };
                    if m.packages.is_empty() {
                        ok_outcome("", "pi", "No pi packages installed. Use `pi install npm:<name>`.", started.elapsed().as_millis() as u64)
                    } else {
                        let mut lines = Vec::new();
                        for pkg in &m.packages {
                            lines.push(format!("  {} {} ({} skills, {} prompts, {} extensions)",
                                pkg.name, pkg.version, pkg.skills_installed, pkg.prompts_installed, pkg.extensions_found.len()));
                        }
                        ok_outcome("", "pi", lines.join("\n"), started.elapsed().as_millis() as u64)
                    }
                }
                "remove" => {
                    let name = args.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        return ok_outcome("", "pi", "ERROR: `name` required for remove", started.elapsed().as_millis() as u64);
                    }
                    let mut m = {
                        let p = data_dir.join("pi").join("manifest.json");
                        std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<PiManifest>(&s).ok()).unwrap_or_default()
                    };
                    let before = m.packages.len();
                    m.packages.retain(|p| p.name != name);
                    let removed = before - m.packages.len();
                    if removed > 0 {
                        let p = data_dir.join("pi").join("manifest.json");
                        if let Ok(s) = serde_json::to_string_pretty(&m) {
                            let _ = std::fs::write(&p, &s);
                        }
                        // Remove skills dir
                        let safe = name.replace('@', "").replace('/', "_");
                        let skill_dir = data_dir.join("skills").join(&safe);
                        let _ = std::fs::remove_dir_all(&skill_dir);
                        // Remove package dir
                        let pkg_dir = data_dir.join("pi").join("packages").join(&safe);
                        let _ = std::fs::remove_dir_all(&pkg_dir);
                        ok_outcome("", "pi", format!("Removed {name}"), started.elapsed().as_millis() as u64)
                    } else {
                        ok_outcome("", "pi", format!("Package {name} not found"), started.elapsed().as_millis() as u64)
                    }
                }
                "reload" => {
                    // Re-scan all installed packages and regenerate bridges.
                    let m = {
                        let p = data_dir.join("pi").join("manifest.json");
                        std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<PiManifest>(&s).ok()).unwrap_or_default()
                    };
                    let mut count = 0usize;
                    for pkg in &m.packages {
                        let pkg_dir = {
                            let safe = pkg.name.replace('@', "").replace('/', "_");
                            data_dir.join("pi").join("packages").join(&safe)
                        };
                        if pkg_dir.join("package.json").exists()
                            && let Ok(pkg_json) = std::fs::read_to_string(pkg_dir.join("package.json"))
                                && let Ok(v) = serde_json::from_str::<Value>(&pkg_json) {
                                    let manifest = v.get("pi");
                                    // Scan for extensions and generate bridge
                                    if let Some(pi) = manifest
                                        && let Some(exts) = pi.get("extensions").and_then(|e| e.as_array()) {
                                            for ext in exts {
                                                if let Some(ext_path) = ext.as_str()
                                                    && generate_bridge(&data_dir, &pkg.name, &pkg_dir, ext_path).await {
                                                        count += 1;
                                                    }
                                            }
                                        }
                                    // Fallback: scan conventional dirs
                                    if manifest.is_none() {
                                        // Check for conventional extensions/ directory
                                        let ext_dir = pkg_dir.join("extensions");
                                        if ext_dir.is_dir()
                                            && let Ok(entries) = std::fs::read_dir(&ext_dir) {
                                                for e in entries.flatten() {
                                                    let p = e.path();
                                                    if p.extension().map(|s| s == "ts" || s == "js").unwrap_or(false)
                                                        && generate_bridge(&data_dir, &pkg.name, &pkg_dir, &p.to_string_lossy()).await {
                                                            count += 1;
                                                        }
                                                }
                                            }
                                    }
                                }
                    }
                    ok_outcome("", "pi", format!("Reloaded {count} extension bridge(s)"), started.elapsed().as_millis() as u64)
                }
                _ => ok_outcome("", "pi", format!("Unknown action: {action}"), started.elapsed().as_millis() as u64),
            }
        })
    }
}

/// Parse a pi package spec into (source_type, package_name, raw_spec).
/// source_type is one of "npm" | "git" | "path"; raw_spec is the spec with
/// any source prefix stripped (what npm pack / git clone actually receives).
fn parse_pi_spec(spec: &str) -> (String, String, String) {
    let (source_type, pkg_spec) = if let Some(rest) = spec.strip_prefix("npm:") {
        ("npm", rest)
    } else if let Some(rest) = spec.strip_prefix("git:") {
        ("git", rest)
    } else if spec.starts_with('/') || spec.starts_with('.') {
        ("path", spec)
    } else {
        ("npm", spec) // default to npm
    };

    let pkg_name = if source_type == "npm" {
        if let Some(rest) = pkg_spec.strip_prefix('@') {
            // "@scope/name" or "@scope/name@ver"
            let slash = rest.find('/').unwrap_or(rest.len());
            let after_slash = &rest[slash + 1..];
            let name_part = after_slash.split('@').next().unwrap_or(after_slash);
            format!("@{}/{}", &rest[..slash], name_part)
        } else {
            pkg_spec.split('@').next().unwrap_or(pkg_spec).to_string()
        }
    } else if source_type == "git" {
        pkg_spec.split('/').next_back().unwrap_or("unknown")
            .split('.').next().unwrap_or("unknown")
            .to_string()
    } else {
        // local path: use dir name
        std::path::Path::new(pkg_spec)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    };

    (source_type.to_string(), pkg_name.trim().to_string(), pkg_spec.to_string())
}

/// Install a pi package and extract its resources.
async fn install_pi_package(data_dir: &Path, spec: &str) -> ToolOutcome {
    let started = std::time::Instant::now();

    // Parse spec and determine package name
    let (source_type, pkg_name, pkg_spec) = parse_pi_spec(spec);
    if pkg_name.is_empty() || pkg_name == "unknown" {
        return ok_outcome("", "pi", format!("Could not determine package name from {spec}"), started.elapsed().as_millis() as u64);
    }

    // Set up target directory
    let safe_name = pkg_name.replace('@', "").replace('/', "_");
    let pkg_dir = data_dir.join("pi").join("packages").join(&safe_name);
    let _ = std::fs::create_dir_all(&pkg_dir);

    // Download/extract the package
    let mut output = Vec::new();
    let mut errors = Vec::new();

    match source_type.as_str() {
        "npm" => {
            // Use npm pack to get the tarball
            let tmp = std::env::temp_dir().join(format!("byteai_pi_{}", std::process::id()));
            let _ = std::fs::create_dir_all(&tmp);
            let tarball_path = tmp.join("package.tgz");

            // npm pack --pack-destination <tmp> <spec>
            let pack_result = Command::new("npm")
                .args(["pack", "--pack-destination", &tmp.to_string_lossy(), &pkg_spec])
                .output()
                .await;

            match pack_result {
                Ok(out) if out.status.success() => {
                    // npm pack may output the filename to stdout
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let tarball = if tarball_path.exists() {
                        tarball_path.clone()
                    } else {
                        // Try to find the actual tarball name from stdout
                        let name = stdout.trim();
                        let named = tmp.join(name);
                        if named.exists() { named } else {
                            // Fallback: look for any .tgz in tmp
                            let mut found = None;
                            if let Ok(entries) = std::fs::read_dir(&tmp) {
                                for e in entries.flatten() {
                                    let p = e.path();
                                    if p.extension().map(|s| s == "tgz").unwrap_or(false) {
                                        found = Some(p);
                                        break;
                                    }
                                }
                            }
                            found.unwrap_or(tarball_path.clone())
                        }
                    };

                    if tarball.exists() {
                        // Extract
                        let extract_result = Command::new("tar")
                            .args(["-xzf", &tarball.to_string_lossy(), "-C", &pkg_dir.to_string_lossy()])
                            .output()
                            .await;

                        match extract_result {
                            Ok(ext) if ext.status.success() => {
                                output.push(format!("Downloaded {pkg_name} successfully"));
                                // The tarball extracts to `package/` subdirectory
                                let extracted = pkg_dir.join("package");
                                if extracted.is_dir() {
                                    // Move contents up
                                    if let Ok(entries) = std::fs::read_dir(&extracted) {
                                        for e in entries.flatten() {
                                            let name = e.file_name();
                                            let target = pkg_dir.join(&name);
                                            let _ = std::fs::rename(e.path(), &target);
                                        }
                                    }
                                    let _ = std::fs::remove_dir(&extracted);
                                }
                            }
                            Ok(ext) => {
                                errors.push(format!("tar extract failed: {}", String::from_utf8_lossy(&ext.stderr)));
                            }
                            Err(e) => {
                                errors.push(format!("tar error: {e}"));
                            }
                        }
                    } else {
                        errors.push(format!("npm pack did not produce a tarball at {tarball_path:?}"));
                    }
                    // Clean up temp
                    let _ = std::fs::remove_dir_all(&tmp);
                }
                Ok(out) => {
                    errors.push(format!("npm pack failed: {}", String::from_utf8_lossy(&out.stderr)));
                }
                Err(e) => {
                    errors.push(format!("npm not found: {e}. Install Node.js/npm to use pi packages."));
                }
            }
        }
        "git" => {
            // Clone from git
            let _ = std::fs::remove_dir_all(&pkg_dir);
            let clone_result = Command::new("git")
                .args(["clone", "--depth", "1", &pkg_spec, &pkg_dir.to_string_lossy()])
                .output()
                .await;

            match clone_result {
                Ok(out) if out.status.success() => {
                    output.push(format!("Cloned {pkg_name} from git"));
                }
                Ok(out) => {
                    errors.push(format!("git clone failed: {}", String::from_utf8_lossy(&out.stderr)));
                }
                Err(e) => {
                    errors.push(format!("git not found: {e}"));
                }
            }
        }
        "path" => {
            // Copy from local path
            let src = std::path::Path::new(&pkg_spec);
            if src.is_dir() {
                // Use cp -R
                let cp_result = Command::new("cp")
                    .args(["-R", &src.to_string_lossy(), &pkg_dir.to_string_lossy()])
                    .output()
                    .await;

                match cp_result {
                    Ok(out) if out.status.success() => {
                        output.push(format!("Copied {pkg_name} from local path"));
                    }
                    Ok(out) => {
                        errors.push(format!("cp failed: {}", String::from_utf8_lossy(&out.stderr)));
                    }
                    Err(e) => {
                        errors.push(format!("cp error: {e}"));
                    }
                }
            } else {
                errors.push(format!("Path not found: {pkg_spec}"));
            }
        }
        _ => {
            errors.push(format!("Unknown source type: {source_type}"));
        }
    }

    if !errors.is_empty() {
        return ok_outcome("", "pi", format!("Errors installing {pkg_name}:\n{}", errors.join("\n")), started.elapsed().as_millis() as u64);
    }

    // Read package.json for the pi manifest
    let pkg_json_path = pkg_dir.join("package.json");
    let pkg_json = match std::fs::read_to_string(&pkg_json_path) {
        Ok(s) => s,
        Err(_) => {
            return ok_outcome("", "pi", format!("Installed {pkg_name} but no package.json found in {pkg_dir:?}"), started.elapsed().as_millis() as u64);
        }
    };

    let pkg: Value = match serde_json::from_str(&pkg_json) {
        Ok(v) => v,
        Err(e) => {
            return ok_outcome("", "pi", format!("Invalid package.json for {pkg_name}: {e}"), started.elapsed().as_millis() as u64);
        }
    };

    let version = pkg.get("version").and_then(Value::as_str).unwrap_or("0.0.0").to_string();
    let pi_manifest = pkg.get("pi");

    // Extract resources
    let mut skills_installed = 0usize;
    let mut prompts_installed = 0usize;
    let mut extensions_found = Vec::new();

    // --- Skills ---
    // From pi manifest or conventional skills/ dir
    let skill_sources: Vec<String> = if let Some(pi) = pi_manifest {
        pi.get("skills")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    } else {
        // Conventional skills/
        let conv = pkg_dir.join("skills");
        if conv.is_dir() {
            vec!["./skills".to_string()]
        } else {
            vec![]
        }
    };

    for source in &skill_sources {
        let source_path = if let Some(rel) = source.strip_prefix("./") {
            pkg_dir.join(rel)
        } else {
            PathBuf::from(source)
        };
        if source_path.is_dir() {
            skills_installed += scan_and_copy_skills(&source_path, &data_dir.join("skills").join(&safe_name));
        }
    }

    // --- Prompts ---
    let prompt_sources: Vec<String> = if let Some(pi) = pi_manifest {
        pi.get("prompts")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    } else {
        let conv = pkg_dir.join("prompts");
        if conv.is_dir() {
            vec!["./prompts".to_string()]
        } else {
            vec![]
        }
    };

    for source in &prompt_sources {
        let source_path = if let Some(rel) = source.strip_prefix("./") {
            pkg_dir.join(rel)
        } else {
            PathBuf::from(source)
        };
        if source_path.is_dir()
            && let Ok(entries) = std::fs::read_dir(&source_path) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().map(|s| s == "md").unwrap_or(false) {
                        let target = data_dir.join("prompts").join(&safe_name).join(e.file_name());
                        let _ = std::fs::create_dir_all(target.parent().unwrap());
                        let _ = std::fs::copy(&p, &target);
                        prompts_installed += 1;
                    }
                }
            }
    }

    // --- Extensions ---
    let ext_sources: Vec<String> = if let Some(pi) = pi_manifest {
        pi.get("extensions")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    } else {
        let conv = pkg_dir.join("extensions");
        if conv.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&conv) {
                let v: Vec<String> = entries.flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().map(|s| s == "ts" || s == "js").unwrap_or(false) {
                            Some(format!("./extensions/{}", e.file_name().to_string_lossy()))
                        } else { None }
                    })
                    .collect();
                if v.is_empty() { vec![] } else { v }
            } else { vec![] }
        } else { vec![] }
    };

    for ext in &ext_sources {
        extensions_found.push(ext.clone());
        // Generate Node bridge for this extension
        generate_bridge(data_dir, &pkg_name, &pkg_dir, ext).await;
    }

    // Record in manifest
    let mut m: PiManifest = {
        let p = data_dir.join("pi").join("manifest.json");
        std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    };

    // Remove existing entry for this package (update)
    m.packages.retain(|p| p.name != pkg_name);

    m.packages.push(PiPackage {
        name: pkg_name.clone(),
        spec: spec.to_string(),
        version,
        skills_installed,
        prompts_installed,
        extensions_found: extensions_found.clone(),
    });

    {
        let p = data_dir.join("pi").join("manifest.json");
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(&m) {
            let _ = std::fs::write(&p, &s);
        }
    }

    let mut result = format!("Installed {pkg_name} ({skills_installed} skills, {prompts_installed} prompts, {} extensions)", extensions_found.len());
    if !extensions_found.is_empty() {
        result.push_str("\nExtensions require Node.js and the pi SDK. Use `pi reload` to regenerate bridges.");
    }
    ok_outcome("", "pi", result, started.elapsed().as_millis() as u64)
}

/// Scan a directory for SKILL.md files and copy them into the target skills dir.
/// Returns the number of skills installed.
fn scan_and_copy_skills(src: &Path, target: &Path) -> usize {
    let mut count = 0;
    if !src.is_dir() {
        return 0;
    }
    if let Ok(entries) = std::fs::read_dir(src) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let skill_md = p.join("SKILL.md");
                if skill_md.exists() {
                    // Copy the entire directory as a skill
                    let name = e.file_name().to_string_lossy().to_string();
                    let skill_target = target.join(&name);
                    let _ = std::fs::create_dir_all(&skill_target);
                    if let Ok(entries2) = std::fs::read_dir(&p) {
                        for e2 in entries2.flatten() {
                            let fname = e2.file_name();
                            let _ = std::fs::copy(e2.path(), skill_target.join(&fname));
                        }
                    }
                    count += 1;
                } else {
                    // Recurse (max depth 2)
                    count += scan_and_copy_skills(&p, target);
                }
            } else if p.extension().map(|s| s == "md").unwrap_or(false) {
                // Top-level .md files are also skills
                let name = e.file_name().to_string_lossy().to_string();
                let skill_target = target.join(name.replace(".md", ""));
                let _ = std::fs::create_dir_all(&skill_target);
                let _ = std::fs::copy(&p, skill_target.join("SKILL.md"));
                count += 1;
            }
        }
    }
    count
}

/// Generate a Node.js bridge shim that loads a pi extension and exposes its
/// tools over stdio JSON-RPC. Returns true if the bridge was generated.
async fn generate_bridge(data_dir: &Path, pkg_name: &str, pkg_dir: &Path, ext_path: &str) -> bool {
    let safe_name = pkg_name.replace('@', "").replace('/', "_");
    let bridge_dir = data_dir.join("pi").join("bridges").join(&safe_name);
    let _ = std::fs::create_dir_all(&bridge_dir);

    // Resolve the extension's absolute path relative to the package dir
    let ext_abs = if let Some(rel) = ext_path.strip_prefix("./") {
        pkg_dir.join(rel)
    } else {
        PathBuf::from(ext_path)
    };

    if !ext_abs.exists() {
        // Try resolving relative to package dir
        let alt = pkg_dir.join(ext_path);
        if !alt.exists() {
            return false;
        }
    }

    let ext_abs = if ext_abs.exists() { ext_abs } else { pkg_dir.join(ext_path) };

    let bridge_js = format!(
        r#"// Pi extension bridge for ByteAI — auto-generated by `pi install`
// Loads the pi extension, captures tool registrations, and serves them
// over stdio JSON-RPC (MCP-compatible protocol).
//
// Usage: node {bridge_name} <args>
//   --list-tools   : print registered tool definitions as JSON
//   --call <json>  : invoke a tool with the given JSON arguments
import {{ createRequire }} from 'node:module';
import {{ resolve, dirname }} from 'node:path';
import {{ fileURLToPath }} from 'node:url';
import fs from 'node:fs';

const PKG_DIR = {pkg_dir_quoted};
const EXT_PATH = {ext_abs_quoted};

// Mini ExtensionAPI shim that captures tool registrations
const tools = [];
const __require = createRequire(PKG_DIR + '/');

// Try to load jiti or tsx for TypeScript support
let loadModule;
try {{
  const jiti = __require('jiti')(PKG_DIR, {{ interopDefault: true }});
  loadModule = (p) => jiti(p);
}} catch {{
  try {{
    // Fallback: use dynamic import for .mjs, require for .js
    loadModule = (p) => {{
      if (p.endsWith('.ts')) throw new Error('TypeScript requires jiti or tsx');
      return __require(p);
    }};
  }} catch {{
    loadModule = null;
  }}
}}

const api = {{
  registerTool(def) {{
    tools.push({{
      name: def.name,
      description: def.description || '',
      parameters: def.parameters || {{ type: 'object', properties: {{}} }},
      _execute: def.execute,
    }});
  }},
  registerCommand(name, cmd) {{ /* stub — commands are not exposed as tools */ }},
  registerProvider(name, cfg) {{ /* stub */ }},
  on(event, handler) {{ /* stub */ }},
  registerShortcut() {{}},
  registerFlag() {{}},
  registerMessageRenderer() {{}},
  appendEntry() {{}},
  setStatus() {{}},
  setWidget() {{}},
  setActiveTools() {{}},
  setModel() {{}},
  setThinkingLevel() {{}},
  setHeader() {{}},
  setFooter() {{}},
  setSessionName() {{}},
  events: {{ on() {{}}, emit() {{}} }},
}};

async function loadExtensions() {{
  if (!loadModule) {{
    process.stderr.write('ERROR: No module loader available. Install jiti or tsx.\n');
    return;
  }}
  try {{
    const mod = loadModule(EXT_PATH);
    const factory = mod.default || mod;
    if (typeof factory === 'function') {{
      await Promise.resolve(factory(api));
    }}
  }} catch (e) {{
    process.stderr.write(`EXTENSION LOAD ERROR: ${{e.message}}\\n`);
  }}
}}

// CLI mode
const args = process.argv.slice(2);
if (args.includes('--list-tools')) {{
  await loadExtensions();
  process.stdout.write(JSON.stringify(tools.map(t => ({{
    name: t.name,
    description: t.description,
    parameters: t.parameters,
  }}))) + '\\n');
  process.exit(0);
}}

if (args[0] === '--call' && args[1]) {{
  await loadExtensions();
  try {{
    const call = JSON.parse(args[1]);
    const tool = tools.find(t => t.name === call.name);
    if (!tool) {{
      process.stdout.write(JSON.stringify({{ error: `Tool "${{call.name}}" not found` }}) + '\\n');
      process.exit(1);
    }}
    // Minimal ExtensionContext for execute
    const ctx = {{
      ui: {{ confirm: async () => true, select: async () => null, input: async () => '', notify: () => {{}} }},
      signal: {{ aborted: false }},
    }};
    const result = await tool._execute(call.id || '0', call.arguments || {{}}, ctx.signal, () => {{}}, ctx);
    process.stdout.write(JSON.stringify(result) + '\\n');
    process.exit(0);
  }} catch (e) {{
    process.stdout.write(JSON.stringify({{ error: e.message, details: e.stack }}) + '\\n');
    process.exit(1);
  }}
}}

// STDIO JSON-RPC mode (MCP-compatible)
await loadExtensions();
const toolsList = tools;

process.stdin.on('data', async (chunk) => {{
  try {{
    const req = JSON.parse(chunk.toString());
    const id = req.id || 1;
    if (req.method === 'tools/list') {{
      process.stdout.write(JSON.stringify({{
        jsonrpc: '2.0', id,
        result: {{ tools: toolsList.map(t => ({{ name: t.name, description: t.description, inputSchema: t.parameters }})) }}
      }}) + '\\n');
    }} else if (req.method === 'tools/call') {{
      const params = req.params || {{}};
      const tool = toolsList.find(t => t.name === params.name);
      if (!tool) {{
        process.stdout.write(JSON.stringify({{
          jsonrpc: '2.0', id,
          error: {{ code: -32601, message: `Tool "${{params.name}}" not found` }}
        }}) + '\\n');
        return;
      }}
      try {{
        const ctx = {{ ui: {{ confirm: async () => true, select: async () => null, input: async () => '', notify: () => {{}} }}, signal: {{ aborted: false }} }};
        const result = await tool._execute(params.name, params.arguments || {{}}, ctx.signal, () => {{}}, ctx);
        process.stdout.write(JSON.stringify({{
          jsonrpc: '2.0', id,
          result: {{ content: Array.isArray(result?.content) ? result.content : [{{ type: 'text', text: JSON.stringify(result) }}] }}
        }}) + '\\n');
      }} catch (e) {{
        process.stdout.write(JSON.stringify({{
          jsonrpc: '2.0', id,
          error: {{ code: -32603, message: e.message }}
        }}) + '\\n');
      }}
    }} else {{
      process.stdout.write(JSON.stringify({{
        jsonrpc: '2.0', id,
        error: {{ code: -32601, message: `Unknown method: ${{req.method}}` }}
      }}) + '\\n');
    }}
  }} catch (e) {{
    process.stderr.write(`pi-bridge parse error: ${{e.message}}\\n`);
  }}
}});
"#,
        bridge_name = bridge_dir.join("bridge.mjs").to_string_lossy(),
        pkg_dir_quoted = serde_json::to_string(&pkg_dir.to_string_lossy()).unwrap_or_default(),
        ext_abs_quoted = serde_json::to_string(&ext_abs.to_string_lossy()).unwrap_or_default(),
    );

    let bridge_path = bridge_dir.join("bridge.mjs");
    let _ = std::fs::write(&bridge_path, &bridge_js);

    // Also create a package.json for the bridge so require() resolution works
    let bridge_pkg = json!({
        "name": format!("byteai-pi-bridge-{safe_name}"),
        "private": true,
        "type": "module",
        "dependencies": {
            "jiti": "^2.0.0"
        }
    });
    let _ = std::fs::write(bridge_dir.join("package.json"), serde_json::to_string_pretty(&bridge_pkg).unwrap_or_default());

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pi_spec_npm_plain() {
        let (st, name, raw) = parse_pi_spec("npm:ponytail");
        assert_eq!((st.as_str(), name.as_str()), ("npm", "ponytail"));
        assert_eq!(raw, "ponytail");
    }

    #[test]
    fn parse_pi_spec_npm_scoped() {
        let (st, name, raw) = parse_pi_spec("npm:@dietrichgebert/ponytail");
        assert_eq!((st.as_str(), name.as_str()), ("npm", "@dietrichgebert/ponytail"));
        assert_eq!(raw, "@dietrichgebert/ponytail");
    }

    #[test]
    fn parse_pi_spec_npm_scoped_with_version() {
        let (st, name, raw) = parse_pi_spec("npm:@dietrichgebert/ponytail@4.9.0");
        assert_eq!((st.as_str(), name.as_str()), ("npm", "@dietrichgebert/ponytail"));
        assert_eq!(raw, "@dietrichgebert/ponytail@4.9.0");
    }

    #[test]
    fn parse_pi_spec_npm_plain_with_version() {
        let (st, name, raw) = parse_pi_spec("npm:bigpowers@2.87.5");
        assert_eq!((st.as_str(), name.as_str()), ("npm", "bigpowers"));
        assert_eq!(raw, "bigpowers@2.87.5");
    }

    #[test]
    fn parse_pi_spec_defaults_to_npm() {
        let (st, name, _raw) = parse_pi_spec("bigpowers");
        assert_eq!((st.as_str(), name.as_str()), ("npm", "bigpowers"));
    }

    #[test]
    fn parse_pi_spec_git() {
        let (st, name, _raw) = parse_pi_spec("git:github.com/user/ponytail");
        assert_eq!(st, "git");
        assert_eq!(name, "ponytail");
    }

    #[test]
    fn parse_pi_spec_path() {
        let (st, name, _raw) = parse_pi_spec("/Users/me/ponytail");
        assert_eq!(st, "path");
        assert_eq!(name, "ponytail");
    }
}