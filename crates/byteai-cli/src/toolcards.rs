//! Tool activity cards — the ByteAi signature look for tool-call sections.
//!
//! Every time the agent uses a tool, its outcome is rendered as a compact
//! "activity card": a per-tool emoji icon, a color-coded status, humanized
//! timing, a smart one-line preview, and (in the TUI) an expandable full
//! view. Both the CLI (`byteai chat` / REPL) and the TUI share this module so
//! the two surfaces always match — the CLI gets bordered ANSI boxes (when the
//! terminal is a TTY), the TUI gets ratatui-style cards with focus + expand.
//!
//! Everything here is pure `std` (no ratatui / crossterm) so the module
//! compiles in every feature configuration.

/// Humanized elapsed time: `0 ms`, `441 ms`, `1.5 s`, `2m 05s`.
pub fn fmt_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        let m = ms / 60_000;
        let s = (ms % 60_000) / 1000;
        format!("{m}m {s:02}s")
    }
}

/// Humanized byte size: `512 B`, `11 KB`, `1.2 MB`.
pub fn fmt_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// ByteAi sigil — a hand-crafted monochrome rune for each tool.
///
/// Deliberately NOT emoji: emoji are colorful clip-art that render
/// differently across terminals (macOS Terminal vs iTerm2 vs tmux), break
/// in 16-color/grayscale modes, and make every tool blend together. The
/// sigils are a single terminal-native glyph per tool, drawn from a
/// cohesive geometric/dingbat family so the whole set reads as one
/// designed system — ByteAi's own. Unknown tools get the action rune `⌁`.
pub fn tool_sigil(name: &str) -> &'static str {
    match name {
        "shell" => "❯",          // prompt chevron
        "read" => "≡",           // lines of text
        "search" => "⌕",         // search rune
        "edit" => "✎",           // pencil
        "memory" => "◈",         // memory cell (diamond)
        "todo" => "▣",           // checked-off square
        "note" => "❝",           // quote mark
        "plan" => "◧",           // grid square (a plan)
        "verify" => "◎",         // bullseye (the verification gate)
        "debug" => "◉",          // target
        "lsp" => "⌘",            // command/symbols
        "skills" => "✦",         // sparkle (a skill)
        "spawn" => "◇",          // child node (empty diamond)
        "review" => "◐",         // half-filled (in review)
        "plugin" => "⊞",         // plug-in square
        "fetch" => "⇣",          // download
        "websearch" => "⌖",      // position/search crosshair
        "graph" => "⌬",          // molecule/graph
        "route" => "≫",          // forward
        "council" => "❖",        // ornate (deliberation)
        "govern" => "⊚",         // circled ring (law)
        "git" => "≋",            // diff tildes
        "sandbox" => "◊",        // lozenge (a sandbox)
        "crew" => "◆",           // solid diamond (a team)
        "mcp" => "⊡",            // connected square
        "schedule" => "◷",       // clock
        "cron" => "◶",           // clock variant
        "workflow" => "⇄",       // cycle
        "improve" => "↗",        // upward trend
        "gates" => "▥",          // ledger square
        "ideas" => "✧",          // sparkle variant (a new idea)
        "github" => "✪",         // starred repo
        "backup" => "◫",         // snapshot square
        "worktree" => "⊟",       // branch (squared minus)
        "secrets" => "❐",        // keyhole page
        "kanban" => "▨",         // columned square
        "moa" => "✺",            // many-voiced star
        "notify" => "❢",         // exclamation ornament
        "proc" => "▰",           // running block
        "terminal" => "❯_",      // persistent prompt with cursor
        "goal" => "◎→",          // target with forward arrow
        "feedback" => "❝★",      // quote + star (human judgment)
        "autoskill" => "✧↻",     // self-evolving spark
        "conductor" => "⌬⊟",     // orchestrated graph
        "autocontext" => "◫◍",   // managed context blocks
        "sessionsearch" => "◔",  // browsing history
        "memsearch" => "◍",      // dotted memory
        "pi" => "π",             // pi
        "dan" | "dan_methodology" => "❋", // eight-fold method
        "herdr" => "✳",          // herd of spokes
        "failover" => "⇌",       // swap arrows
        _ => "⌁",                // generic action
    }
}

/// True when stdout is an interactive terminal (enables ANSI in the CLI).
pub fn stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Best-effort terminal width for CLI boxes (COLUMNS env, fallback 80).
pub fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(80)
        .clamp(40, 220)
}

/// A smart preview of a tool's output.
pub struct Preview {
    /// Up to `max_lines` non-empty lines, each capped at `max_line_chars`
    /// with a trailing ellipsis when truncated on a line.
    pub lines: Vec<String>,
    /// Total line count of the raw output (empty/whitespace lines included).
    pub total_lines: usize,
    /// Total byte size of the raw output.
    pub total_bytes: usize,
    /// Bytes of the *shown* preview lines (so the footer can report what's
    /// hidden).
    pub shown_bytes: usize,
}

/// Build a smart preview: the first few non-empty lines, capped in length,
/// plus bookkeeping so callers can render a `… +N lines · +X` footer.
pub fn preview(output: &str, max_lines: usize, max_line_chars: usize) -> Preview {
    let total_bytes = output.len();
    let total_lines = output.lines().count().max(1);
    let mut lines = Vec::new();
    let mut shown_bytes = 0usize;
    for line in output.lines().filter(|l| !l.trim().is_empty()).take(max_lines) {
        let line = line.trim_end();
        if line.chars().count() > max_line_chars {
            let t: String = line.chars().take(max_line_chars).collect();
            shown_bytes += t.len();
            lines.push(format!("{t}…"));
        } else {
            shown_bytes += line.len();
            lines.push(line.to_string());
        }
    }
    // An output that is entirely blank still gets one "empty" slot.
    if lines.is_empty() && total_bytes > 0 {
        lines.push("…".to_string());
    }
    Preview { lines, total_lines, total_bytes, shown_bytes }
}

/// Paint a single segment with an ANSI code (no-op when `ansi` is false).
fn paint(code: &str, s: &str, ansi: bool) -> String {
    if ansi {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Hard-wrap a string to `width` character cells (emoji/CJK = 2 cells).
fn wrap_to(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cells = 0usize;
    for ch in s.chars() {
        let cw = char_width(ch);
        if cells + cw > width {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cells = 0;
        }
        cur.push(ch);
        cells += cw;
        if cells > width {
            // A single character wider than the box (long emoji/ZWJ): flush.
            out.push(std::mem::take(&mut cur));
            cells = 0;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// ANSI escape constants for CLI cards.
const B_GREEN: &str = "32";
const B_RED: &str = "31";
const B_YELLOW: &str = "33";
const B_CYAN: &str = "36";
const B_GRAY: &str = "90";

/// Render one tool call as a bordered "activity card" for the CLI.
///
/// `width` = box width in cells (`0` = auto-detect from the terminal). The
/// box shows a header row (`💻 shell ✓ 441 ms`), the first few lines of
/// output (wrapped), and a footer with what was hidden. When the output is
/// empty the card collapses to a single header row. `ansi` gates color.
pub fn cli_card(name: &str, ok: bool, elapsed_ms: u64, output: &str, width: usize, ansi: bool) -> String {
    // Box chars (╭ │ ╮ ╰ ╯ ─) are 1 cell wide: a content line is
    // `│ ` (2) + inner + ` │` (2) = inner + 4 cells, so inner = width - 4.
    let width = if width == 0 { term_width() } else { width };
    let inner = width.saturating_sub(4).max(8);

    let icon = tool_sigil(name);
    let status = if ok { "✓" } else { "✗" };
    let status_code = if ok { B_GREEN } else { B_RED };
    let mut head = format!(
        "{} {} {} {}",
        icon,
        paint(BOLD, name, ansi),
        paint(status_code, status, ansi),
        fmt_elapsed(elapsed_ms),
    );
    // Cyan size chip (matches the TUI card header).
    if !output.is_empty() {
        head.push_str(&format!(" · {}", paint(B_CYAN, &fmt_bytes(output.len()), ansi)));
    }
    let head_w = display_width(&head);

    let mut out = String::new();
    // Top border: ╭─ <head> ────────────────╮   (total = inner + 4 = width)
    let pad = inner.saturating_sub(head_w);
    let mut top = String::from("╭─");
    top.push_str(&head);
    if pad > 0 {
        top.push(' ');
        top.push_str(&"─".repeat(pad));
    }
    top.push('╮');
    out.push_str(&paint(B_GRAY, &trim_to_width(&top, width), ansi));
    out.push('\n');

    let p = preview(output, 3, inner.saturating_sub(2).max(20));
    for line in &p.lines {
        for wl in wrap_to(line, inner) {
            out.push_str(&paint(B_GRAY, &format!("│ {wl:<width$} │", wl = wl, width = inner), ansi));
            out.push('\n');
        }
    }

    // Footer: hidden count (lines beyond preview) + hidden bytes + failed hint.
    let hidden_lines = p.total_lines.saturating_sub(p.lines.len());
    let hidden_bytes = p.total_bytes.saturating_sub(p.shown_bytes);
    let mut foot = String::new();
    if hidden_lines > 0 {
        foot.push_str(&format!("… +{hidden_lines} line{}", if hidden_lines == 1 { "" } else { "s" }));
    }
    if hidden_bytes > 0 {
        if !foot.is_empty() {
            foot.push_str(" · ");
        }
        foot.push_str(&paint(B_CYAN, &fmt_bytes(hidden_bytes), ansi));
    }
    if !ok {
        if !foot.is_empty() {
            foot.push_str(" · ");
        }
        foot.push_str(&paint(B_YELLOW, "⚠ failed", ansi));
    }
    // Bottom border: ╰─ <foot> ──────────────╯   (total = foot_w + inner + 6 - foot_w ... = width)
    let foot_w = display_width(&foot);
    let mut bottom = String::from("╰─");
    if foot.is_empty() {
        bottom.push_str(&"─".repeat(inner + 1));
    } else {
        bottom.push(' ');
        bottom.push_str(&foot);
        bottom.push(' ');
        bottom.push_str(&"─".repeat(inner.saturating_sub(foot_w + 1)));
    }
    bottom.push('╯');
    out.push_str(&paint(B_GRAY, &trim_to_width(&bottom, width), ansi));

    out
}

/// Strip ANSI SGR escape sequences (`\x1b[...m`) for width measurement.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            for c2 in chars.by_ref() {
                if c2 == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Unicode character width in terminal cells (East Asian Width heuristic).
/// Box-drawing chars (U+2500–U+257F) are 1; CJK, emoji, hangul are 2.
fn char_width(c: char) -> usize {
    let code = c as u32;
    if (0x1100..=0x115F).contains(&code)
        || code == 0x2329
        || code == 0x232A
        || (0x2E80..=0x303E).contains(&code)
        || (0x3040..=0x33FF).contains(&code)
        || (0x3400..=0x4DBF).contains(&code)
        || (0x4E00..=0x9FFF).contains(&code)
        || (0xA000..=0xA4CF).contains(&code)
        || (0xAC00..=0xD7AF).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0xFE10..=0xFE19).contains(&code)
        || (0xFE30..=0xFE6F).contains(&code)
        || (0xFF01..=0xFF60).contains(&code)
        || (0xFFE0..=0xFFE6).contains(&code)
        || (0x1B000..=0x1B0FF).contains(&code)
        || (0x1F000..=0x1FFFF).contains(&code)
        || (0x20000..=0x2FFFF).contains(&code)
        || (0x30000..=0x3FFFF).contains(&code)
    {
        2
    } else {
        1
    }
}

/// Approximate display width in terminal cells (ANSI escapes ignored).
fn display_width(s: &str) -> usize {
    strip_ansi(s).chars().map(char_width).sum()
}

/// Trim a box line to `width` cells so an over-long header can't overflow.
/// ANSI escapes are copied through untouched (they take no cells).
fn trim_to_width(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cells = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            let mut esc = String::from("\x1b[");
            chars.next(); // consume '['
            for c2 in chars.by_ref() {
                esc.push(c2);
                if c2 == 'm' {
                    break;
                }
            }
            out.push_str(&esc);
            continue;
        }
        let cw = char_width(c);
        if cells + cw > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        cells += cw;
    }
    format!("{out}╯")
}

const BOLD: &str = "1";

/// btop-style timing sparkline of tool-call durations (▁▂▃▄▅▆▇█),
/// normalized to the slowest call so the shape shows the turn's rhythm.
/// Empty when there are no timings to draw.
pub fn sparkline(durations: &[u64]) -> String {
    let max = durations.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return String::new();
    }
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    durations
        .iter()
        .map(|d| {
            let h = if *d == 0 {
                0
            } else {
                ((*d as f64 / max as f64) * 7.0).round() as usize
            };
            BARS[h.clamp(0, 7)]
        })
        .collect()
}

/// One-line turn summary (the "ribbon"): which tools ran (as ByteAi sigils),
/// how long the whole turn took, and a sparkline of the tool-call rhythm.
/// Names are collapsed to the first 6 + "+N more".
pub fn ribbon(tools: &[(String, u64)], total_ms: u64) -> String {
    let names: Vec<String> = tools
        .iter()
        .map(|(n, _)| format!("{} {n}", tool_sigil(n)))
        .collect();
    let shown: Vec<String> = if names.len() <= 6 {
        names.clone()
    } else {
        let mut v: Vec<String> = names[..6].to_vec();
        v.push(format!("… +{} more", names.len() - 6));
        v
    };
    let timings: Vec<u64> = tools.iter().map(|(_, e)| *e).collect();
    let sp = sparkline(&timings);
    format!(
        "⟫ {} tool call{} · {} · {}{}",
        tools.len(),
        if tools.len() == 1 { "" } else { "s" },
        shown.join("  "),
        fmt_elapsed(total_ms),
        if sp.is_empty() { String::new() } else { format!("  {sp}") },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formats_humanly() {
        assert_eq!(fmt_elapsed(0), "0 ms");
        assert_eq!(fmt_elapsed(441), "441 ms");
        assert_eq!(fmt_elapsed(1500), "1.5 s");
        assert_eq!(fmt_elapsed(125_000), "2m 05s");
    }

    #[test]
    fn bytes_formats_humanly() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KB");
        assert_eq!(fmt_bytes(11_385), "11.1 KB");
        assert_eq!(fmt_bytes(1_500_000), "1.4 MB");
    }

    #[test]
    fn preview_caps_lines_and_chars() {
        let out = "a\n\nbb\nccc\ndddd";
        let p = preview(out, 2, 3);
        assert_eq!(p.lines, vec!["a".to_string(), "bb".to_string()]);
        assert_eq!(p.total_lines, 5);
        assert_eq!(p.total_bytes, out.len());
        assert!(p.shown_bytes < p.total_bytes);
    }

    #[test]
    fn preview_truncates_long_line_with_ellipsis() {
        let out = format!("{}tail", "x".repeat(200));
        let p = preview(&out, 1, 20);
        assert_eq!(p.lines.len(), 1);
        assert!(p.lines[0].ends_with('…'));
        assert!(p.lines[0].chars().count() <= 21);
    }

    #[test]
    fn preview_empty_output_is_safe() {
        let p = preview("", 3, 80);
        assert!(p.lines.is_empty());
        assert_eq!(p.total_bytes, 0);
    }

    #[test]
    fn sigils_cover_known_tools_and_default() {
        assert_eq!(tool_sigil("shell"), "❯");
        assert_eq!(tool_sigil("memory"), "◈");
        assert_eq!(tool_sigil("gates"), "▥");
        assert_eq!(tool_sigil("totally_unknown"), "⌁");
        // Sigils are monochrome runes, never color emoji.
        for (_name, sigil) in [("shell", "❯"), ("pi", "π"), ("dan", "❋")] {
            assert!(!sigil.contains("💻") && !sigil.contains("🧠") && !sigil.contains("🥋"), "no emoji");
            let _ = sigil;
        }
    }

    #[test]
    fn cli_card_is_bounded_and_has_header() {
        let card = cli_card("shell", true, 441, "hello world\nsecond line\nthird line\nfourth", 60, false);
        assert!(card.contains("❯"));
        assert!(card.contains("✓"));
        assert!(card.contains("441 ms"));
        assert!(card.starts_with("╭─"));
        assert!(card.contains("+1 line"));
        // Every line must fit inside the box width (60 cells).
        for line in card.lines() {
            assert!(display_width(line) <= 60, "line too wide: {line:?}");
        }
    }

    #[test]
    fn cli_card_failed_shows_warning() {
        let card = cli_card("shell", false, 12, "ERROR: boom", 60, false);
        assert!(card.contains("✗"));
        assert!(card.contains("failed"));
    }

    #[test]
    fn cli_card_empty_output_is_single_row() {
        let card = cli_card("note", true, 0, "", 60, false);
        // Header + closing border only.
        assert_eq!(card.lines().count(), 2, "{card}");
    }

    #[test]
    fn ribbon_reports_tools_and_time() {
        let tools = vec![
            ("memory".to_string(), 1),
            ("shell".to_string(), 2),
            ("todo".to_string(), 3),
        ];
        let r = ribbon(&tools, 1500);
        assert!(r.contains("3 tool calls"));
        assert!(r.contains("◈"));
        assert!(r.contains("❯"));
        assert!(r.contains("1.5 s"));
    }

    #[test]
    fn sparkline_shapes_by_relative_duration() {
        // All-equal durations → flat line of identical bars.
        let flat = sparkline(&[100, 100, 100]);
        assert_eq!(flat.chars().count(), 3);
        assert!(flat.chars().all(|c| c == flat.chars().next().unwrap()), "flat: {flat}");
        // The longest call gets the tallest bar (█).
        let shape = sparkline(&[1, 500, 100]);
        assert_eq!(shape.chars().count(), 3);
        assert_eq!(shape.chars().nth(1), Some('█'), "max duration must be █: {shape}");
        // Zero-length timings produce no sparkline at all.
        assert_eq!(sparkline(&[0, 0]), "");
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn ribbon_collapses_many_tools() {
        let tools: Vec<(String, u64)> = (0..10).map(|i| (format!("tool{i}"), i)).collect();
        let r = ribbon(&tools, 100);
        assert!(r.contains("+4 more"));
    }
}
