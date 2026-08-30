# Contributing to ByteAi

Thanks for your interest in ByteAi! We welcome contributions of all kinds —
features, bug fixes, documentation, tests, and ideas.

## Development Setup

```sh
git clone https://github.com/ByteCyberSec/byteai && cd byteai
cargo build
cargo test
```

## Code Style

- Format with `cargo fmt`
- Lint with `cargo clippy -- -D warnings`
- Follow the existing crate structure (`crates/byteai-*`)
- Keep the agent fast — cold start < 100 ms and low RAM are core values

## Testing

```sh
cargo test          # unit + integration tests
cargo test --release
```

Every change must keep the test suite green. Add tests for new tools and
commands (see `crates/byteai-tools/src/*.rs` `#[cfg(test)]` modules for the
pattern).

## Adding a New Tool

1. Create `crates/byteai-tools/src/<name>.rs`
2. Implement the `Tool` trait (`name`, `def`, `execute`)
3. Register it in `crates/byteai-tools/src/lib.rs`
4. Add unit tests in the same file
5. Add a toolcard sigil in `crates/byteai-cli/src/toolcards.rs` (monochrome, no emoji)

## Adding a Slash Command

1. Add the command to the TUI palette `COMMANDS` in `crates/byteai-cli/src/tui.rs`
2. Handle it in the TUI `handle_command` match
3. Add it to the REPL match in `crates/byteai-cli/src/main.rs`
4. Keep `every_palette_command_is_handled` test green

## Commit Convention

We use conventional commits:

- `feat(...)` — new feature
- `fix(...)` — bug fix
- `chore(...)` — maintenance
- `docs(...)` — documentation

## Pull Requests

- One logical change per PR
- Reference the issue/phase it addresses
- Include tests for behavior changes
- Keep the README's phase table updated when you land a phase

## Questions?

Open an issue. We respond fast.