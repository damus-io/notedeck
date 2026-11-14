//! How `nd <ns> <verb>` runs a tool that [streams](ToolInfo::streams): it
//! subscribes over the socket and shows each event as it arrives, until
//! notedeck ends the stream or the person interrupts. nd knows nothing of
//! any one stream: the server renders each event's text, and nd only shows
//! it the way the tool's [`StreamMode`] says.
//!
//! - **Append** (`nd agentium log <session> -f`): each event's text follows
//!   the last, as `tail -f` prints.
//! - **Redraw** (`nd agentium watch`): each event's text is the whole
//!   screen. At a terminal it is drawn in place on the alternate screen, each
//!   line clipped to the terminal's width, and Ctrl-C hands the screen back
//!   as it was; into a pipe each frame follows the last after a blank line.
//! - **`--json`**: each event's value, one compact line apiece, either mode.
//!
//! Color is kept for a terminal and stripped otherwise, as for a call. A
//! reader that goes away (`… | head`) ends the stream quietly.
//!
//! [`ToolInfo::streams`]: notedeck_ipc::ToolInfo::streams

use crate::Failure;
use crate::output::{Output, strip_color};
use crate::start::Reach;
use notedeck_ipc::{Request, StreamMode};
use serde_json::Value;
use std::io::{self, IsTerminal, Write};

/// Subscribe to `tool` with `args` and show its events as `mode` and
/// `output` say, until the stream ends.
#[cfg(unix)]
pub fn follow(
    reach: &Reach,
    tool: String,
    args: Value,
    mode: StreamMode,
    output: Output,
) -> Result<(), Failure> {
    use notedeck_ipc::{Outcome, Reply};

    let request = Request::Subscribe { name: tool, args };
    let version = request.version();
    let mut client = reach.subscribe(&request)?;
    let tty = io::stdout().is_terminal();
    let mut view = View {
        mode,
        json: output.json,
        color: output.color.enabled(tty),
        width: tty.then(terminal_width).flatten(),
        screen: None,
        tty,
        frames: 0,
    };
    loop {
        let response = client
            .next_response()
            .map_err(|e| Failure::new(format!("reading the stream: {e}")))?;
        let Some(response) = response else {
            return Ok(());
        };
        let reply = match response.outcome {
            Outcome::Ok(reply) => reply,
            Outcome::Error { message } => return Err(Failure::new(message)),
        };
        if response.v != version {
            return Err(Failure::new(format!(
                "notedeck {} answered a stream in protocol version {}, and nd follows streams in {version}",
                response.notedeck, response.v
            )));
        }
        let Reply::Event { event, text } = reply else {
            return Err(Failure::new(
                "notedeck answered a subscribe with something other than an event",
            ));
        };
        if view.show(&event, text.as_deref()).is_err() {
            // Whoever read stdout has gone; so has the point of following.
            return Ok(());
        }
    }
}

/// There is no socket off Unix, so nothing to subscribe to: the call says so.
#[cfg(not(unix))]
pub fn follow(
    reach: &Reach,
    tool: String,
    args: Value,
    _mode: StreamMode,
    _output: Output,
) -> Result<(), Failure> {
    reach
        .exchange(&Request::Subscribe { name: tool, args })
        .map(|_| ())
}

/// Where a stream's events are shown, and how.
struct View {
    mode: StreamMode,
    /// `--json`: each event's value rather than its text.
    json: bool,
    color: bool,
    /// The terminal's width, to clip a redrawn frame's lines to; `None` into
    /// a pipe.
    width: Option<usize>,
    /// The alternate screen, once the first frame is drawn on a terminal.
    screen: Option<Screen>,
    /// Whether stdout is a terminal.
    tty: bool,
    /// How many frames have gone into a pipe, to set each off from the last.
    frames: usize,
}

impl View {
    /// Show one event. An error means stdout is gone.
    fn show(&mut self, event: &Value, text: Option<&str>) -> io::Result<()> {
        let mut out = io::stdout().lock();
        let Some(text) = text.filter(|_| !self.json) else {
            writeln!(out, "{event}")?;
            return out.flush();
        };
        let text = match self.color {
            true => std::borrow::Cow::Borrowed(text),
            false => strip_color(text),
        };
        match self.mode {
            StreamMode::Append => out.write_all(text.as_bytes())?,
            StreamMode::Redraw if self.tty => {
                let screen = self.screen.get_or_insert_with(Screen::enter);
                screen.draw(&mut out, &clip_lines(&text, self.width))?;
            }
            StreamMode::Redraw => {
                if self.frames > 0 {
                    writeln!(out)?;
                }
                out.write_all(text.as_bytes())?;
                self.frames += 1;
            }
        }
        out.flush()
    }
}

/// The alternate screen with the cursor hidden, held while a redrawing
/// stream runs. Dropping it, or Ctrl-C, gives the person their screen back.
struct Screen;

/// Leave the alternate screen and show the cursor again.
const LEAVE_SCREEN: &str = "\x1b[?25h\x1b[?1049l";

impl Screen {
    /// Switch to the alternate screen, hiding the cursor, with Ctrl-C (or a
    /// `kill`) leaving it again before nd exits.
    fn enter() -> Self {
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
        let _ = out.flush();
        on_interrupt_leave_screen(true);
        Screen
    }

    /// Replace the screen's contents with `frame`: home the cursor, clear to
    /// the end, then draw.
    fn draw(&self, out: &mut impl Write, frame: &str) -> io::Result<()> {
        write!(out, "\x1b[H\x1b[J{frame}")
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        on_interrupt_leave_screen(false);
        let mut out = io::stdout().lock();
        let _ = out.write_all(LEAVE_SCREEN.as_bytes());
        let _ = out.flush();
    }
}

/// While `on`, SIGINT and SIGTERM leave the alternate screen before exiting
/// as they would have (130 and 143); off, they do what they did before.
#[cfg(unix)]
fn on_interrupt_leave_screen(on: bool) {
    extern "C" fn leave_and_exit(signal: libc::c_int) {
        // Only async-signal-safe calls here: a raw write and `_exit`.
        // SAFETY: writing a static buffer to stdout's descriptor, then
        // exiting without running anything else.
        unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                LEAVE_SCREEN.as_ptr().cast(),
                LEAVE_SCREEN.len(),
            );
            libc::_exit(128 + signal);
        }
    }
    let handler = match on {
        true => leave_and_exit as extern "C" fn(libc::c_int) as libc::sighandler_t,
        false => libc::SIG_DFL,
    };
    // SAFETY: installing a handler that only makes async-signal-safe calls,
    // or putting back the default.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// No signals to catch off Unix: Ctrl-C leaves the screen as it is.
#[cfg(not(unix))]
fn on_interrupt_leave_screen(_on: bool) {}

/// The width to clip a frame to: `$COLUMNS` when it is a positive number,
/// else the terminal's own.
fn terminal_width() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .or_else(stdout_width)
}

/// The width of the terminal on stdout, in columns, or `None` when stdout
/// isn't one.
#[cfg(unix)]
fn stdout_width() -> Option<usize> {
    // SAFETY: TIOCGWINSZ only writes a `winsize` into the struct passed, and
    // fails harmlessly (returning -1) when stdout isn't a terminal.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_col > 0).then_some(ws.ws_col as usize)
}

#[cfg(not(unix))]
fn stdout_width() -> Option<usize> {
    None
}

/// `text` with each line clipped to `width` visible columns, or whole when
/// there is no width. A line drawn past the edge would wrap and push the
/// frame off the screen it is redrawn on.
fn clip_lines(text: &str, width: Option<usize>) -> String {
    let Some(width) = width else {
        return text.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        clip_line(body, width, &mut out);
        out.push_str(newline);
    }
    out
}

/// Append `line` to `out`, cut after `width` visible characters. Escape
/// sequences (`ESC [ … letter`) take no width and are kept whole; a cut line
/// that carried any ends with a reset, so its color can't bleed on.
fn clip_line(line: &str, width: usize, out: &mut String) {
    let mut shown = 0;
    let mut colored = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            colored = true;
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if shown == width {
            if colored {
                out.push_str("\x1b[0m");
            }
            return;
        }
        out.push(c);
        shown += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_clipped_to_the_width_keeping_its_color() {
        assert_eq!(clip_lines("abcdef\nab\n", Some(4)), "abcd\nab\n");
        assert_eq!(clip_lines("abcdef", None), "abcdef");
        assert_eq!(
            clip_lines("\x1b[33mabcdef\x1b[0m\n", Some(3)),
            "\x1b[33mabc\x1b[0m\n",
            "the escape takes no width, and the cut resets the color"
        );
        assert_eq!(clip_lines("○ é\n", Some(3)), "○ é\n", "a char is a column");
    }
}
