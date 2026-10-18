//! A review commit's message, read for the review pane: the body under the
//! subject (the record already carries the subject) with its git trailers
//! split out, parsed once when the load lands so the pane only draws slices.
//!
//! Commit bodies are hard-wrapped at ~72 columns for a terminal. The pane is
//! wider or narrower than that, so prose paragraphs are reflowed into single
//! lines that egui wraps at the pane's width. Bullet lists and indented lines
//! (code, aligned output) keep their line breaks, since there the breaks mean
//! something.
//!
//! The message is also kept as written ([`CommitBody::raw`]), for the pane's
//! `m` toggle: there the author's own layout — ASCII tables, wrapping, the
//! exact trailers — is the point.

/// A commit message's body and trailers, reflowed.
#[derive(Debug, Default)]
pub(crate) struct CommitBody {
    /// The body without the subject or the trailers: paragraphs separated by a
    /// blank line, each prose paragraph on one line.
    body: String,
    /// The trailer block (`Changelog-Fixed: …`, `Headway: …`), one per line as
    /// written; empty when the message has none.
    trailers: String,
    /// The message as written, without the subject: hard wraps, lists and
    /// trailers kept, blank lines trimmed at both ends.
    raw: String,
}

impl CommitBody {
    /// Parse `message` (`git log --format=%B`): drop the subject — git's
    /// subject is the first paragraph — split off a trailing trailer block,
    /// and reflow what's left.
    pub(crate) fn parse(message: &str) -> Self {
        let mut paragraphs = paragraphs(message).skip(1).collect::<Vec<_>>();
        let trailers = match paragraphs.pop() {
            Some(last) if is_trailer_block(&last) => last.join("\n"),
            Some(last) => {
                paragraphs.push(last);
                String::new()
            }
            None => String::new(),
        };

        let mut body = String::new();
        for (i, paragraph) in paragraphs.iter().enumerate() {
            if i > 0 {
                body.push_str("\n\n");
            }
            reflow(paragraph, &mut body);
        }
        let raw = raw_body(message);
        Self {
            body,
            trailers,
            raw,
        }
    }

    /// Whether there's nothing under the subject: the pane draws nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.body.is_empty() && self.trailers.is_empty()
    }

    /// The body, reflowed, without the subject or the trailers; empty when
    /// the message has none.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// The trailer block, one per line; empty when there is none.
    pub(crate) fn trailers(&self) -> &str {
        &self.trailers
    }

    /// The message as written, without the subject (the header shows it):
    /// hard wraps kept and the trailers in place; empty when the message is
    /// only a subject.
    pub(crate) fn raw(&self) -> &str {
        &self.raw
    }
}

/// `message` without its subject — the first paragraph, as [`paragraphs`]
/// reads it — and without blank lines at either end; every other line as
/// written, trailing whitespace trimmed.
fn raw_body(message: &str) -> String {
    let blank = |l: &&str| l.trim_end().is_empty();
    let mut lines = message
        .lines()
        .skip_while(blank)
        .skip_while(|l| !blank(l))
        .skip_while(blank)
        .map(str::trim_end)
        .collect::<Vec<_>>();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// `message`'s paragraphs, each its lines with trailing whitespace trimmed;
/// runs of blank lines separate them.
fn paragraphs(message: &str) -> impl Iterator<Item = Vec<&str>> {
    let mut lines = message.lines().map(str::trim_end).peekable();
    std::iter::from_fn(move || {
        while lines.next_if(|l| l.is_empty()).is_some() {}
        let paragraph = std::iter::from_fn(|| lines.next_if(|l| !l.is_empty())).collect::<Vec<_>>();
        (!paragraph.is_empty()).then_some(paragraph)
    })
}

/// Whether `paragraph` is a git trailer block: every line a `Token: value`
/// (a token of letters, digits and dashes, so a prose line like `Worth
/// noting: …` isn't one), or an indented continuation of the line before.
fn is_trailer_block(paragraph: &[&str]) -> bool {
    let Some(first) = paragraph.first() else {
        return false;
    };
    is_trailer(first)
        && paragraph[1..]
            .iter()
            .all(|line| is_trailer(line) || line.starts_with([' ', '\t']))
}

/// Whether `line` is one `Token: value` trailer. The value may be empty, as
/// in `Changelog-None:`.
fn is_trailer(line: &str) -> bool {
    let Some((token, value)) = line.split_once(':') else {
        return false;
    };
    !token.is_empty()
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && (value.is_empty() || value.starts_with(' '))
}

/// Append `paragraph` to `out`, joining its prose lines with a space and
/// keeping list items and indented lines on lines of their own, as written.
fn reflow(paragraph: &[&str], out: &mut String) {
    let mut prose = false;
    for (i, line) in paragraph.iter().enumerate() {
        let kept = is_list_item(line) || line.starts_with([' ', '\t']);
        if i > 0 {
            out.push(if prose && !kept { ' ' } else { '\n' });
        }
        out.push_str(line);
        prose = !kept;
    }
}

/// Whether `line` starts a bullet (`- `, `* `) or numbered (`1. `, `1) `)
/// list item.
fn is_list_item(line: &str) -> bool {
    if line.starts_with("- ") || line.starts_with("* ") {
        return true;
    }
    let rest = line.trim_start_matches(|c: char| c.is_ascii_digit());
    rest.len() < line.len() && (rest.starts_with(". ") || rest.starts_with(") "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subject alone (with or without a trailing newline) has no body.
    #[test]
    fn subject_only_is_empty() {
        for message in ["headway: a subject", "headway: a subject\n", ""] {
            let body = CommitBody::parse(message);
            assert!(body.is_empty(), "{message:?}");
            assert_eq!(body.body(), "");
            assert_eq!(body.raw(), "");
        }
    }

    /// The raw body keeps the hard wraps, the list and the trailers as
    /// written, and drops the subject and the blank lines around it.
    #[test]
    fn raw_keeps_the_message_as_written() {
        let body = CommitBody::parse(
            "headway: subject\n\
             that wraps\n\
             \n\
             \n\
             The review pane shows only the subject and drops\n\
             the body, which is where the reasoning lives.\n\
             - one item\n\
             \n\
             Changelog-Fixed: the pane drops the body\n\
             Headway: headway:headway/skirt-laugh-domain\n\
             \n",
        );
        assert_eq!(
            body.raw(),
            "The review pane shows only the subject and drops\n\
             the body, which is where the reasoning lives.\n\
             - one item\n\
             \n\
             Changelog-Fixed: the pane drops the body\n\
             Headway: headway:headway/skirt-laugh-domain"
        );
    }

    /// A message with no trailers gives its body as written, untouched by
    /// the reflow.
    #[test]
    fn raw_without_trailers() {
        let body = CommitBody::parse("subject\n\nFirst line\nsecond line.\n\n\tcode\n");
        assert_eq!(body.trailers(), "");
        assert_eq!(body.body(), "First line second line.\n\n\tcode");
        assert_eq!(body.raw(), "First line\nsecond line.\n\n\tcode");
    }

    /// The trailer block is split off the body, which keeps every paragraph.
    #[test]
    fn body_and_trailers_split() {
        let message = "headway: subject\n\
                       \n\
                       First paragraph.\n\
                       \n\
                       Second paragraph.\n\
                       \n\
                       Changelog-Fixed: the pane drops the body\n\
                       Headway: headway:headway/skirt-laugh-domain\n\
                       Co-Authored-By: Claude <noreply@anthropic.com>\n";
        let body = CommitBody::parse(message);
        assert_eq!(body.body(), "First paragraph.\n\nSecond paragraph.");
        assert_eq!(
            body.trailers(),
            "Changelog-Fixed: the pane drops the body\n\
             Headway: headway:headway/skirt-laugh-domain\n\
             Co-Authored-By: Claude <noreply@anthropic.com>"
        );
    }

    /// A message of a subject and trailers alone has no body text but isn't
    /// empty: it has the trailers. An empty value (`Changelog-None:`) is a
    /// trailer, and a trailer's indented continuation stays in the block.
    #[test]
    fn trailers_without_body() {
        let body = CommitBody::parse("subject\n\nChangelog-None:\nFixes: abc (\"x\n  y\")");
        assert_eq!(body.body(), "");
        assert_eq!(body.trailers(), "Changelog-None:\nFixes: abc (\"x\n  y\")");
        assert!(!body.is_empty());
    }

    /// A hard-wrapped paragraph comes back on one line, so the pane wraps it
    /// at its own width.
    #[test]
    fn hard_wrapped_paragraph_reflows() {
        let body = CommitBody::parse(
            "subject\n\n\
             The review pane shows only the subject and drops\n\
             the body, which is where the reasoning lives.\n",
        );
        assert_eq!(
            body.body(),
            "The review pane shows only the subject and drops the body, which is \
             where the reasoning lives."
        );
    }

    /// List items and indented lines keep their line breaks; prose before the
    /// list in the same paragraph still reflows.
    #[test]
    fn bullet_list_kept() {
        let body = CommitBody::parse(
            "subject\n\n\
             Intro that is\n\
             hard-wrapped:\n\
             - one item\n  \
               continued\n\
             * another\n\
             1. numbered\n\
             2) also numbered\n\n\
             \tindented code\n\
             \tmore code\n",
        );
        assert_eq!(
            body.body(),
            "Intro that is hard-wrapped:\n\
             - one item\n  \
               continued\n\
             * another\n\
             1. numbered\n\
             2) also numbered\n\n\
             \tindented code\n\
             \tmore code"
        );
        assert_eq!(body.trailers(), "");
    }

    /// A last paragraph that only looks like a trailer — prose with a colon,
    /// whose "token" has a space in it — stays in the body.
    #[test]
    fn prose_last_paragraph_stays_body() {
        let body = CommitBody::parse(
            "subject\n\nFirst.\n\nWorth noting: the cache is cold on first open.\n",
        );
        assert_eq!(body.trailers(), "");
        assert_eq!(
            body.body(),
            "First.\n\nWorth noting: the cache is cold on first open."
        );
    }
}
