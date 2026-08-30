//! LSP integration tests — spawn real servers, exercise the wire protocol.
//! Gated on the server binary being on PATH; otherwise skipped.

use std::path::Path;
use std::time::Duration;

use crate::*;

fn tmp_file(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("byteai_lsp_test");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

/// Full flow in ONE process: spawn clangd → open → wait diagnostics → symbols
/// → hover → definition. Keeps the server alive across all calls.
#[tokio::test]
async fn clangd_full_flow() {
    if !command_on_path("clangd") {
        eprintln!("skipping: clangd not on PATH");
        return;
    }
    let registry = LspRegistry::new(default_servers());
    let file = tmp_file(
        "flow.c",
        "int helper(int a, int b) { return a + b; }\nint main(void) { return helper(1, 2); }\n",
    );
    let text = std::fs::read_to_string(&file).unwrap();
    let root = file.parent().unwrap();

    // Get the server and keep it alive for the whole test sequence.
    let state = registry.get("c", root).await.unwrap();
    let mut st = state.lock().await;
    let s = match &mut *st {
        ServerState::Ready(s) => s,
        _ => panic!("server not ready"),
    };

    // 1) Open file, wait for diagnostics (clean).
    s.did_open(&file, &text).await.unwrap();
    s.did_change(&file, &text, 1).await.unwrap();
    let diags = s.wait_diagnostics(&file, Duration::from_secs(10)).await;
    assert!(diags.is_empty(), "expected clean file, got: {diags:?}");

    // 2) Symbols
    let syms = s.document_symbols(&file).await.unwrap();
    assert!(
        syms.iter().any(|sym| sym.name == "helper" || sym.name == "main"),
        "symbols: {syms:?}"
    );

    // 3) Hover on `helper` (line 0, col 5)
    let hover = s.hover(&file, 0, 5).await.unwrap();
    assert!(hover.contains("int"), "hover should describe helper, got: {hover:?}");

    // 4) Definition of `helper` at its use site (line 1, col 24 = 'h' of helper)
    let defs = s.definition(&file, 1, 24).await.unwrap();
    assert!(!defs.is_empty(), "definition should resolve helper");
    assert!(
        defs[0].uri.contains("flow.c") && defs[0].start_line == 1,
        "definition should point to helper at line 1, got {defs:?}"
    );

    // 5) References of helper (use site at line 2; declaration excluded by
    // includeDeclaration=false)
    let refs = s.references(&file, 0, 5, false).await.unwrap();
    assert!(!refs.is_empty(), "expected >=1 reference, got {refs:?}");
    assert_eq!(refs[0].start_line, 2, "use site should be on line 2, got {refs:?}");
}

/// Diagnostics catch real errors (the EDIT → repair loop core).
#[tokio::test]
async fn clangd_diagnostics_catch_error() {
    if !command_on_path("clangd") {
        eprintln!("skipping: clangd not on PATH");
        return;
    }
    let registry = LspRegistry::new(default_servers());
    let file = tmp_file("err.c", "int main(void) { return \"str\"; }\n");
    let text = std::fs::read_to_string(&file).unwrap();
    let root = file.parent().unwrap();
    let state = registry.get("c", root).await.unwrap();
    let mut st = state.lock().await;
    let s = match &mut *st {
        ServerState::Ready(s) => s,
        _ => panic!("server not ready"),
    };
    s.did_open(&file, &text).await.unwrap();
    s.did_change(&file, &text, 1).await.unwrap();
    let diags = s.wait_diagnostics(&file, Duration::from_secs(10)).await;
    assert!(!diags.is_empty(), "should catch the type error");
    assert!(
        diags.iter().any(|d| d.severity == Some(1)),
        "should include at least one error, got {diags:?}"
    );
}

#[test]
fn framing_roundtrip() {
    let body = json!({ "jsonrpc": "2.0", "method": "x", "params": {} });
    let bytes = serde_json::to_vec(&body).unwrap();
    let frame = encode_frame(&bytes);
    let header = String::from_utf8_lossy(&frame[..frame.len() - bytes.len()]);
    assert!(header.starts_with("Content-Length: "), "header: {header}");
    assert!(frame.ends_with(b"}"), "frame should end with body");
    let parsed: Value = serde_json::from_slice(&frame[frame.len() - bytes.len()..]).unwrap();
    assert_eq!(parsed["method"], "x");
}

#[test]
fn uri_encoding() {
    assert_eq!(path_to_uri(Path::new("/tmp/a b.c")), "file:///tmp/a%20b.c");
    assert_eq!(path_to_uri(Path::new("/tmp/#x.c")), "file:///tmp/%23x.c");
    assert_eq!(path_to_uri(Path::new("/a/b.rs")), "file:///a/b.rs");
}