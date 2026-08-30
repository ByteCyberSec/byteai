# Phase 2 — Code Intelligence (2026-08-25)

## Scope
LSP client, AST symbol extraction, smart reads, symbol search, multi-strategy edit engine, edit+diagnostics repair loop.

## Deliverables
- **byteai-lsp** crate: JSON-RPC 2.0 client over stdio, language server registry, diagnostics/symbols/hover/definition/references/rename/formatting
- **byteai-ast** crate: Tree-sitter symbol extraction for 7 languages (Rust, Python, TS, JS, Go, C, C++)
- Extended **edit** tool: exact → contextual → whole-file strategies, LSP diagnostics validation
- Extended **read** tool: symbols/function/imports AST modes
- Extended **search** tool: `mode=symbol` (AST-based definition search)
- New **lsp** tool: 8 actions with graceful degradation
- **byteai tool** CLI subcommand for direct tool invocation
- **byteai doctor** shows LSP server availability

## Tests
- **26 tests**, all passing: 9 AST + 4 LSP (incl. 2 live clangd integration tests) + 13 tools

## Verification
- LSP symbols on byteai-core: 21 symbols, correct kinds, line numbers
- LSP diagnostics on C error: `1 errors, 0 warnings: E 1:21 Incompatible pointer to integer conversion`
- EDIT → diagnostics → repair loop: edit introduces error → LSP catches it → warns model
- Contextual edit: whitespace-tolerant matching (indented `let   msg   =   "hello"` → matched)
- AST smart reads: symbol outline, function extraction, import listing
- Symbol search: finds `contextual_replace` across 19 files
- Definition/hover/references work in persistent server mode (verified via integration test)

## Binary
16.4 MB (up from 8.6 MB — tree-sitter grammars + LSP crate)

## Known Issues
- `byteai tool lsp` spawns a fresh server per invocation (cold start slow); real agent keeps server alive
- Hover returns empty via CLI tool (per-call server issue); works in integration tests
- OmniRoute completions still 401