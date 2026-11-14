//! How `nd <ns> <verb>` prints a tool's answer.
//!
//! A tool that renders answers with text beside its value
//! (`Reply::Result::text`), drawn on the server, which has the types to draw
//! it. nd prints that text unless asked for `--json`, and the value when there
//! is no text. The text arrives with its muted parts colored; nd keeps the
//! color for a terminal or a pager and strips it otherwise, since only nd
//! knows where its stdout goes.
//!
//! A tool's `notice` (`Reply::Result::notice`) is a line for the person, not
//! the answer, and goes to stderr either way, so the text or the value pipes
//! clean.

use cli_term::{ColorWhen, PagerMode};
use serde_json::Value;
use std::borrow::Cow;
use std::io::IsTerminal;

/// What a generated command's nd-level flags ask of its output: `--json`,
/// `--pager`/`--no-pager` and `--color <when>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    /// Print the value even when the tool rendered text.
    pub json: bool,
    pub pager: PagerMode,
    pub color: ColorWhen,
}

impl Default for Output {
    /// The text, paged and colored when stdout is a terminal.
    fn default() -> Self {
        Self {
            json: false,
            pager: PagerMode::Auto,
            color: ColorWhen::Auto,
        }
    }
}

impl Output {
    /// Print what a tool answered: its `text` unless [`json`](Self::json) or
    /// there is none, else `value`. `ns` is the command's namespace, whose
    /// `$<NS>_PAGER` is tried before `$PAGER`.
    pub fn print(&self, value: &Value, text: Option<&str>, ns: &str) {
        let Some(text) = text.filter(|_| !self.json) else {
            print_json(value);
            return;
        };
        let tty = std::io::stdout().is_terminal();
        let use_pager = self.pager.enabled(tty) && !text.is_empty();
        let text = match self.color.enabled(tty || use_pager) {
            true => Cow::Borrowed(text),
            false => strip_color(text),
        };
        cli_term::emit(&text, use_pager, &pager_var(ns));
    }
}

/// A tool's notice on stderr, muted when stderr is a terminal (as the
/// `headway` CLI printed its `diff` resolution line).
pub fn print_notice(notice: &str) {
    match std::io::stderr().is_terminal() {
        true => eprintln!("\x1b[2m{notice}\x1b[0m"),
        false => eprintln!("{notice}"),
    }
}

/// `value` on stdout: indented for a person at a terminal, one line for a
/// pipe.
pub fn print_json(value: &Value) {
    let text = if std::io::stdout().is_terminal() {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    println!("{}", text.expect("a JSON value always serializes"));
}

/// The variable naming the pager for commands under `ns`: `$HEADWAY_PAGER`
/// for `nd headway …`, the one the `headway` CLI reads.
pub fn pager_var(ns: &str) -> String {
    format!("{}_PAGER", ns.to_ascii_uppercase().replace('-', "_"))
}

/// `text` without its SGR escapes (`ESC [ … m`), which is all the color a
/// tool's text carries. Borrowed when there are none.
pub fn strip_color(text: &str) -> Cow<'_, str> {
    if !text.contains('\x1b') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        // Params are digits and `;`; anything else isn't an SGR, so keep it.
        let params = after
            .find(|c: char| !(c.is_ascii_digit() || c == ';'))
            .unwrap_or(after.len());
        if after[params..].starts_with('m') {
            rest = &after[params + 1..];
        } else {
            out.push_str("\x1b[");
            rest = after;
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_color_drops_sgr_only() {
        assert_eq!(
            strip_color("a  \x1b[90mheadway:b/x\x1b[0m\n"),
            "a  headway:b/x\n"
        );
        assert_eq!(strip_color("\x1b[1;31m!\x1b[0m"), "!");
        assert!(matches!(strip_color("plain"), Cow::Borrowed("plain")));
        // A non-SGR escape is left alone.
        assert_eq!(strip_color("\x1b[2Kx"), "\x1b[2Kx");
    }

    #[test]
    fn pager_var_is_the_namespace_s() {
        assert_eq!(pager_var("headway"), "HEADWAY_PAGER");
        assert_eq!(pager_var("my-app"), "MY_APP_PAGER");
    }
}
