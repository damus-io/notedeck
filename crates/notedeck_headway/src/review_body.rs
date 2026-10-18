//! A review commit's message, read for the review pane: the body under the
//! subject (the record already carries the subject) with its git trailers
//! split out, parsed once when the load lands so the pane only draws slices.
//!
//! Commit bodies are hard-wrapped at ~72 columns for a terminal. The pane is
//! wider or narrower than that, so prose paragraphs are reflowed into single
//! lines that egui wraps at the pane's width. Bullet lists and indented lines
//! (code, aligned output) keep their line breaks, since there the breaks mean
//! something.

/// A commit message's body and trailers, reflowed, with whether the pane shows
/// all of it.
#[derive(Debug, Default)]
pub(crate) struct CommitBody {
    /// The body without the subject or the trailers: paragraphs separated by a
    /// blank line, each prose paragraph on one line.
    body: String,
    /// Where the body's first paragraph ends, as a byte index into
    /// [`body`](Self::body): the collapsed view is `body[..first_end]`.
    first_end: usize,
    /// The trailer block (`Changelog-Fixed: …`, `Headway: …`), one per line as
    /// written; empty when the message has none.
    trailers: String,
    /// Whether the pane shows the whole body and the trailers, or only the
    /// first paragraph.
    pub expanded: bool,
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
        let mut first_end = 0;
        for (i, paragraph) in paragraphs.iter().enumerate() {
            if i > 0 {
                body.push_str("\n\n");
            }
            reflow(paragraph, &mut body);
            if i == 0 {
                first_end = body.len();
            }
        }
        Self {
            body,
            first_end,
            trailers,
            expanded: false,
        }
    }

    /// Whether there's nothing under the subject: the pane draws nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.body.is_empty() && self.trailers.is_empty()
    }

    /// The body as the pane shows it: the first paragraph while collapsed,
    /// all of it once expanded.
    pub(crate) fn shown(&self) -> &str {
        if self.expanded {
            &self.body
        } else {
            &self.body[..self.first_end]
        }
    }

    /// The trailer block, one per line; empty when there is none.
    pub(crate) fn trailers(&self) -> &str {
        &self.trailers
    }

    /// Whether there's more than the first paragraph to show, so the pane
    /// offers its Show more / Show less toggle.
    pub(crate) fn has_more(&self) -> bool {
        self.first_end < self.body.len() || !self.trailers.is_empty()
    }
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

    /// A subject alone (with or without a trailing newline) has no body, and
    /// nothing to expand.
    #[test]
    fn subject_only_is_empty() {
        for message in ["headway: a subject", "headway: a subject\n", ""] {
            let body = CommitBody::parse(message);
            assert!(body.is_empty(), "{message:?}");
            assert!(!body.has_more());
            assert_eq!(body.shown(), "");
        }
    }

    /// The trailer block is split off the body; collapsed shows the first
    /// paragraph, expanded the whole body, the trailers apart from both.
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
        let mut body = CommitBody::parse(message);
        assert_eq!(body.shown(), "First paragraph.");
        assert!(body.has_more());
        body.expanded = true;
        assert_eq!(body.shown(), "First paragraph.\n\nSecond paragraph.");
        assert_eq!(
            body.trailers(),
            "Changelog-Fixed: the pane drops the body\n\
             Headway: headway:headway/skirt-laugh-domain\n\
             Co-Authored-By: Claude <noreply@anthropic.com>"
        );
    }

    /// A message of a subject and trailers alone has no body text but still
    /// something to expand: the trailers. An empty value (`Changelog-None:`)
    /// is a trailer, and a trailer's indented continuation stays in the block.
    #[test]
    fn trailers_without_body() {
        let body = CommitBody::parse("subject\n\nChangelog-None:\nFixes: abc (\"x\n  y\")");
        assert_eq!(body.shown(), "");
        assert_eq!(body.trailers(), "Changelog-None:\nFixes: abc (\"x\n  y\")");
        assert!(!body.is_empty());
        assert!(body.has_more());
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
            body.shown(),
            "The review pane shows only the subject and drops the body, which is \
             where the reasoning lives."
        );
    }

    /// List items and indented lines keep their line breaks; prose before the
    /// list in the same paragraph still reflows.
    #[test]
    fn bullet_list_kept() {
        let mut body = CommitBody::parse(
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
        body.expanded = true;
        assert_eq!(
            body.shown(),
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
        let mut body = CommitBody::parse(
            "subject\n\nFirst.\n\nWorth noting: the cache is cold on first open.\n",
        );
        assert_eq!(body.trailers(), "");
        body.expanded = true;
        assert_eq!(
            body.shown(),
            "First.\n\nWorth noting: the cache is cold on first open."
        );
    }

    /// A one-paragraph body has nothing more to show without trailers.
    #[test]
    fn one_paragraph_has_no_more() {
        let body = CommitBody::parse("subject\n\nJust this.");
        assert_eq!(body.shown(), "Just this.");
        assert!(!body.has_more());
    }
}
