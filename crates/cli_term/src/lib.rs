//! Terminal output shared by the standalone CLIs (`agentium`, `headway`).
//!
//! Lifted out of `agentium_cli` so `headway grep` behaves exactly like
//! `agentium grep` rather than carrying a drifting copy: the `--color` and
//! `--pager`/`--no-pager` modes and how they resolve against a terminal, the
//! pager itself ([`emit`]), SGR painting ([`paint`]), and the smart-case
//! pattern compilation and match highlighting behind `grep`.
//!
//! Std plus `regex` only, so a CLI can depend on it without pulling in any of
//! the other's machinery.

use regex::Regex;

/// Wrap `s` in an SGR color when `enabled`, else return it plain. `sgr` is the
/// numeric code(s), e.g. `"32"` or `"1;31"`.
pub fn paint(enabled: bool, sgr: &str, s: &str) -> String {
    if enabled {
        format!("\x1b[{sgr}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// When to ANSI-color output (`--color`). `Auto` follows the effective sink (a
/// tty or a color-aware pager); `Always`/`Never` force it — `Always` is how you
/// keep color when piping into your own `less -R`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorWhen {
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    /// Parse the `--color` value; anything else is an error naming the choices.
    pub fn parse(s: &str) -> Result<ColorWhen, String> {
        match s {
            "auto" => Ok(ColorWhen::Auto),
            "always" => Ok(ColorWhen::Always),
            "never" => Ok(ColorWhen::Never),
            other => Err(format!("--color must be auto|always|never, got '{other}'")),
        }
    }

    /// Resolve to on/off. `sink_supports_color` is whether the effective output
    /// (tty or color-aware pager) can render ANSI — the `Auto` signal.
    pub fn enabled(self, sink_supports_color: bool) -> bool {
        match self {
            ColorWhen::Auto => sink_supports_color,
            ColorWhen::Always => true,
            ColorWhen::Never => false,
        }
    }
}

/// Whether to page output (`--pager`/`--no-pager`). `Auto` pages only when
/// stdout is a tty (so a pipe stays unpaged); the flags force it either way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagerMode {
    Auto,
    Always,
    Never,
}

impl PagerMode {
    /// Resolve to on/off given whether stdout is a terminal.
    pub fn enabled(self, stdout_tty: bool) -> bool {
        match self {
            PagerMode::Auto => stdout_tty,
            PagerMode::Always => true,
            PagerMode::Never => false,
        }
    }
}

/// Write `output` to stdout, or through a pager when `use_pager`.
///
/// The pager command comes from the CLI's own variable `pager_var` (e.g.
/// `$AGENTIUM_PAGER`), then `$PAGER`, else the built-in default `less -R`
/// (`-R` so the rendered ANSI color survives). If the pager can't be spawned
/// (not installed, empty command), this falls back to printing plainly rather
/// than failing. A broken pipe (the user quit the pager early) is ignored.
pub fn emit(output: &str, use_pager: bool, pager_var: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if !use_pager {
        print!("{output}");
        return;
    }

    let pager = std::env::var(pager_var)
        .ok()
        .or_else(|| std::env::var("PAGER").ok())
        .unwrap_or_else(|| "less -R".to_string());
    let mut parts = pager.split_whitespace();
    let Some(program) = parts.next() else {
        print!("{output}");
        return;
    };

    let child = Command::new(program)
        .args(parts)
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        // No usable pager (e.g. `less` absent) — degrade to a plain print.
        Err(_) => {
            print!("{output}");
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        // Ignore the write result: a pager the user quits early closes the pipe,
        // and that EPIPE is expected, not an error worth surfacing.
        let _ = stdin.write_all(output.as_bytes());
    }
    let _ = child.wait();
}

/// How `grep` decides case sensitivity.
///
/// The default is [`Smart`](CaseMode::Smart) rather than grep(1)'s
/// case-sensitive, because what the CLIs search is prose: the needles worth
/// typing are overwhelmingly names — `Hyrule`, `NoteView`, `RelayPool` —
/// written capitalized in the text and lowercase in the shell. A literal
/// reading of grep(1) answers "no matches" to a search whose subject is
/// everywhere, which is the one answer a search tool must never give wrongly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaseMode {
    /// The default: insensitive unless the pattern itself carries case.
    Smart,
    /// `-i`/`--ignore-case` — always insensitive.
    Insensitive,
    /// `-s`/`--case-sensitive` — always sensitive, grep(1)'s own default, for
    /// when the distinction is the point (`Ndb` the type vs `ndb` the CLI).
    Sensitive,
}

impl CaseMode {
    /// Whether `pattern` should be compiled case-insensitively under this mode.
    fn insensitive_for(self, pattern: &str) -> bool {
        match self {
            CaseMode::Smart => !pattern_carries_case(pattern),
            CaseMode::Insensitive => true,
            CaseMode::Sensitive => false,
        }
    }
}

/// Whether the user spelled case into `pattern` — smart-case's entire signal.
///
/// Only uppercase in the *matched text* counts. An escape carries its uppercase
/// in the syntax instead: `\W` and `\S` are negated classes, and a Unicode class
/// names its property (`\p{Lu}`, `\P{Greek}`) rather than the characters it
/// matches. Reading those as "the user asked for case" would silently make
/// `\w+ error` sensitive, so the scan skips an escaped character, and the braced
/// or single-letter body after `\p`/`\P`.
fn pattern_carries_case(pattern: &str) -> bool {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if c.is_uppercase() {
                return true;
            }
            continue;
        }
        match chars.next() {
            // `\p{Lu}` (braced) or `\pL` (single-letter shorthand).
            Some('p') | Some('P') => {
                if chars.as_str().starts_with('{') {
                    for c in chars.by_ref() {
                        if c == '}' {
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            // Any other escape: the one skipped character is all of it.
            _ => {}
        }
    }
    false
}

/// Compile a `grep` pattern, folding the case decision in as the regex's own
/// case-insensitive flag rather than lowercasing haystack and needle (which
/// would break the highlight offsets, and any pattern that cares about case
/// classes).
///
/// Meant to run during argument parsing, so an unparseable pattern fails
/// immediately, with the regex crate's own diagnostic, instead of after seconds
/// of relay reconcile.
pub fn compile_pattern(pattern: &str, case: CaseMode) -> Result<Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(case.insensitive_for(pattern))
        .build()
        .map_err(|e| format!("invalid search pattern '{pattern}': {e}"))
}

/// Bold red for a matched span — `grep --color`'s own convention.
pub const SGR_MATCH: &str = "1;31";

/// Copy `line`, wrapping every match of `pattern` in [`SGR_MATCH`]. A no-op
/// (returning the line unchanged) when color is off, so the plain output stays
/// byte-for-byte the source text.
///
/// Zero-width matches are skipped rather than painted: a pattern like `a*`
/// matches the empty string at every position, and highlighting those would
/// bury the line in escape codes without marking anything.
pub fn highlight(pattern: &Regex, line: &str, color: bool) -> String {
    if !color {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut end = 0;
    for m in pattern.find_iter(line) {
        if m.start() == m.end() {
            continue;
        }
        out.push_str(&line[end..m.start()]);
        out.push_str(&paint(true, SGR_MATCH, m.as_str()));
        end = m.end();
    }
    out.push_str(&line[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_gates_on_flag() {
        assert_eq!(paint(false, "32", "x"), "x");
        assert_eq!(paint(true, "32", "x"), "\x1b[32mx\x1b[0m");
    }

    #[test]
    fn color_and_pager_modes_resolve() {
        assert_eq!(ColorWhen::parse("always"), Ok(ColorWhen::Always));
        assert!(ColorWhen::parse("sometimes").is_err());
        assert!(ColorWhen::Auto.enabled(true));
        assert!(!ColorWhen::Auto.enabled(false));
        assert!(ColorWhen::Always.enabled(false));
        assert!(!ColorWhen::Never.enabled(true));

        assert!(PagerMode::Auto.enabled(true));
        assert!(!PagerMode::Auto.enabled(false));
        assert!(PagerMode::Always.enabled(false));
        assert!(!PagerMode::Never.enabled(true));
    }

    #[test]
    fn smart_case_folds_only_a_lowercase_pattern() {
        let smart = |p| compile_pattern(p, CaseMode::Smart).unwrap();
        assert!(smart("hyrule").is_match("Hyrule"));
        assert!(!smart("Term.*ux").is_match("terminal ux"));
        assert!(
            compile_pattern("Term.*ux", CaseMode::Insensitive)
                .unwrap()
                .is_match("terminal ux")
        );
        assert!(
            !compile_pattern("hyrule", CaseMode::Sensitive)
                .unwrap()
                .is_match("Hyrule")
        );
        assert!(compile_pattern("(", CaseMode::Smart).is_err());
    }

    #[test]
    fn smart_case_ignores_uppercase_inside_escapes() {
        // `\W`, `\S` and `\p{Lu}` carry their uppercase in the syntax, not in
        // the text they match, so none of them should pin the search.
        for pattern in ["\\Werror", "\\S+ error", "\\p{Lu}error", "\\pLerror"] {
            assert!(
                !pattern_carries_case(pattern),
                "{pattern} should stay case-insensitive under smart-case"
            );
        }
        // A literal uppercase still counts, even next to an escape.
        assert!(pattern_carries_case("\\w+Error"));
        assert!(pattern_carries_case("\\p{Lu}Error"));
    }

    #[test]
    fn highlight_paints_only_real_matches() {
        let re = compile_pattern("cat", CaseMode::Sensitive).unwrap();
        // Color off is byte-for-byte the source line.
        assert_eq!(highlight(&re, "a cat and a cat", false), "a cat and a cat");
        // Color on wraps every occurrence, leaving the rest intact.
        let painted = highlight(&re, "a cat and a cat", true);
        assert_eq!(painted.matches(SGR_MATCH).count(), 2);
        assert!(painted.starts_with("a "));
        assert!(painted.ends_with("\x1b[0m"));
        // A pattern that can match the empty string paints nothing spurious: the
        // zero-width matches are skipped, so the line survives unchanged.
        let star = compile_pattern("x*", CaseMode::Sensitive).unwrap();
        assert_eq!(highlight(&star, "abc", true), "abc");
    }
}
