//! Shared terminal formatting: status glyphs and colors, column padding,
//! relative times and home-abbreviated paths.

/// Amber — the status color for a session waiting on the user, and the color of
/// the summary line that surfaces them.
pub(crate) const SGR_NEEDS_INPUT: &str = "33";
/// Bold, for the per-host group headers.
pub(crate) const SGR_BOLD: &str = "1";

/// Terminal presentation for a status string: a glyph, a human label, and an SGR
/// color. Mirrors [`AgentStatus`] — which lives in the egui-side notedeck_dave
/// crate (its `color()` returns an `egui::Color32`), so it can't be reused from a
/// terminal CLI. An unknown status shows its raw token, uncolored.
///
/// [`AgentStatus`]: https://docs.rs/notedeck_dave
pub(crate) fn status_style(status: &str) -> (&'static str, String, &'static str) {
    match status {
        "idle" => ("○", "Idle".into(), "90"),
        "working" => ("●", "Working".into(), "32"),
        "needs_input" => ("◆", "Needs Input".into(), SGR_NEEDS_INPUT),
        "error" => ("✖", "Error".into(), "31"),
        "done" => ("✓", "Done".into(), "34"),
        "pending" => ("◌", "Pending".into(), "36"),
        "deleted" => ("⊘", "Deleted".into(), "90"),
        other => ("?", other.to_string(), "0"),
    }
}

/// Replace a leading home directory with `~`, matching how the desktop shows
/// working directories.
pub(crate) fn abbreviate_home(cwd: &str, home: &str) -> String {
    match cwd.strip_prefix(home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => cwd.to_string(),
    }
}

/// A coarse "2h ago" for an event timestamp, relative to `now` (both Unix secs).
pub(crate) fn relative_time(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86400),
    }
}

/// Truncate `s` to `width` display chars (appending `…` when cut) and left-pad
/// to `width` so columns align. ANSI color must be applied *after* this, or the
/// invisible escape bytes would throw the padding off.
pub(crate) fn col(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > width {
        let mut t: String = chars[..width.saturating_sub(1)].iter().collect();
        t.push('…');
        return t;
    }
    format!("{s:<width$}")
}

/// SGR painting is shared with the other CLIs (see [`cli_term::paint`]); it is
/// re-exported here beside the rest of this crate's terminal formatting.
pub(crate) use cli_term::paint;

/// The current Unix time in seconds.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Clip an already-painted `line` to `width` visible chars, for a dashboard that
/// must not wrap. SGR escapes (`\x1b[…m`) take no columns, so they are copied
/// through uncounted; a clipped line ends in `…` and, when it carried color, a
/// reset so the cut can't leak a color into the next line.
pub(crate) fn fit(line: &str, width: usize) -> String {
    if visible_len(line) <= width {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut visible = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            copy_escape(&mut chars, Some(&mut out));
            continue;
        }
        // Keep the last column for the ellipsis.
        if visible + 1 == width {
            break;
        }
        out.push(c);
        visible += 1;
    }
    out.push('…');
    if line.contains('\x1b') {
        out.push_str("\x1b[0m");
    }
    out
}

/// How many columns `line` occupies: its chars, less any SGR escapes.
fn visible_len(line: &str) -> usize {
    let mut n = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            copy_escape(&mut chars, None);
        } else {
            n += 1;
        }
    }
    n
}

/// Consume the rest of an escape sequence (the `ESC` already taken) up to and
/// including its final letter, appending it to `out` when given.
fn copy_escape(chars: &mut std::str::Chars, mut out: Option<&mut String>) {
    for e in chars.by_ref() {
        if let Some(out) = out.as_deref_mut() {
            out.push(e);
        }
        if e.is_ascii_alphabetic() {
            break;
        }
    }
}

/// The width of the terminal on stdout, in columns, or `None` when stdout isn't
/// a terminal (or the platform can't say).
#[cfg(unix)]
pub(crate) fn stdout_width() -> Option<usize> {
    // SAFETY: TIOCGWINSZ only writes a `winsize` into the struct we pass, and
    // fails harmlessly (returning -1) when stdout isn't a terminal.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_col > 0).then_some(ws.ws_col as usize)
}

/// The width of the terminal on stdout. Not queried off unix, where `$COLUMNS`
/// is the only source.
#[cfg(not(unix))]
pub(crate) fn stdout_width() -> Option<usize> {
    None
}

/// Takes over the terminal for a redraw-in-place view: switches to the
/// alternate screen and hides the cursor on creation, and restores both when
/// dropped — so a Ctrl-C that breaks the caller's loop (or an error that
/// unwinds out of it) still hands the user back their shell as it was.
pub(crate) struct ScreenGuard;

impl ScreenGuard {
    /// Enter the alternate screen with the cursor hidden.
    pub(crate) fn enter() -> ScreenGuard {
        write_raw("\x1b[?1049h\x1b[?25l");
        ScreenGuard
    }

    /// Replace the screen's contents with `frame`: home the cursor, clear to the
    /// end, then draw.
    pub(crate) fn draw(&self, frame: &str) {
        write_raw(&format!("\x1b[H\x1b[J{frame}"));
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        write_raw("\x1b[?25h\x1b[?1049l");
    }
}

/// Write `s` to stdout and flush, ignoring errors (a closed terminal has nobody
/// left to tell).
fn write_raw(s: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_pads_and_truncates() {
        assert_eq!(col("hi", 5), "hi   ");
        assert_eq!(col("exactly", 7), "exactly");
        // longer than width: cut to width-1 chars plus an ellipsis
        assert_eq!(col("toolongword", 5), "tool…");
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(100, 100), "0s ago");
        assert_eq!(relative_time(100, 90), "10s ago");
        assert_eq!(relative_time(60, 0), "1m ago");
        assert_eq!(relative_time(3600, 0), "1h ago");
        assert_eq!(relative_time(90_000, 0), "1d ago");
        // 'then' in the future clamps rather than underflows.
        assert_eq!(relative_time(0, 100), "0s ago");
    }

    #[test]
    fn abbreviate_home_replaces_prefix() {
        assert_eq!(abbreviate_home("/home/u/proj", "/home/u"), "~/proj");
        assert_eq!(abbreviate_home("/other/x", "/home/u"), "/other/x");
        assert_eq!(abbreviate_home("/home/u/proj", ""), "/home/u/proj");
    }

    #[test]
    fn status_style_known_and_unknown() {
        let (g, l, c) = status_style("needs_input");
        assert_eq!((g, l.as_str(), c), ("◆", "Needs Input", SGR_NEEDS_INPUT));
        // A tombstoned session gets its own muted glyph rather than the "?" fallback.
        let (g, l, _) = status_style("deleted");
        assert_eq!((g, l.as_str()), ("⊘", "Deleted"));
        let (g, l, c) = status_style("weird");
        assert_eq!((g, l.as_str(), c), ("?", "weird", "0"));
    }

    #[test]
    fn fit_counts_only_visible_chars() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 5), "hello");
        assert_eq!(fit("hello world", 5), "hell…");
        // Escapes are free: a painted word that fits is untouched…
        let painted = paint(true, "32", "hi");
        assert_eq!(fit(&painted, 2), painted);
        // …and a clipped painted line is reset so the color can't bleed.
        assert_eq!(fit(&paint(true, "32", "hello"), 3), "\x1b[32mhe…\x1b[0m");
    }
}
