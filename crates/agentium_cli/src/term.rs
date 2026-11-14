//! Terminal formatting. The pure part (status glyphs and colors, column
//! padding, relative times, home-abbreviated paths) lives in
//! [`agentium_core::cli::term`], shared with the nd tools; this module adds
//! what only a process on a terminal can do: measure it and redraw it.

pub(crate) use agentium_core::cli::term::{
    SGR_BOLD, StatusStyle, now_secs, paint, relative_time, status_style,
};

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
