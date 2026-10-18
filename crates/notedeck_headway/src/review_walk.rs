//! A review record's explainer sources, read for the review pane: the
//! explainer's Markdown (`<word-id>.md`) as a page, and the code walk
//! (`<word-id>.code.md`) threaded into the commit's diff.
//!
//! Both are parsed once, when the source (or the diff it is threaded into)
//! changes, and kept: the pane only draws what is here.
//!
//! The walk is prose and ` ```hunk ` fences, as `explain` writes it:
//!
//! ````text
//! The opening paragraph: what the change does, read first.
//!
//! ```hunk apps/api/src/admin/admin-sso.guard.ts @@ -41,6 +41,14 @@
//! match: if (isLocal() && !msAccount)
//! ```
//!
//! Why the guard moved: the note for that hunk, after it.
//! ````
//!
//! The prose after a fence, up to the next, is that hunk's note; the prose
//! before the first fence is the walk's opening. The fence is found in the
//! diff the way `explain` finds it ([`find_hunk`]): by path and header, and by
//! its `match:` line when a rebase moved the header. Its text is never shown.
//!
//! A source is the record's own sealed copy when it has one, or else the
//! commit's explain git note ([`source_text`]): a colleague's commit, or one
//! recorded before records carried sources. A squashed commit's note holds
//! two pages joined by a blank line; it is drawn whole, so the second page's
//! front matter shows as text.

use std::borrow::Cow;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};

use headway::event::{ReviewSourceRole, ReviewView};
use headway::git::{CommitNotes, NoteKind};
use nostrdb_net::NoteId;
use notedeck_ui::diff::{GitPatch, Hunk, LineKind};

use crate::explain_palette::Palette;
use crate::review_figures::{Figured, Figures};

/// The explainer's front matter the pane shows as its header, and the keys
/// that pick its palette for figures. The rest of it (fonts, sidecars) is for
/// the built page and is skipped.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FrontMatter {
    /// The page's name.
    pub title: Option<String>,
    /// What it's about: `repo · path · sha`.
    pub src: Option<String>,
    /// The header's chips, each a short claim with a number in it.
    pub facts: Vec<String>,
    /// The accent's hue (degrees or a name): moves brass.
    pub accent: Option<String>,
    /// The ground (`cool`, `warm`, `neutral`, `paper`, or a hue): tints the
    /// neutrals.
    pub ground: Option<String>,
    /// `hues:`'s entries, each moving one role's hue.
    pub hues: Vec<HueOverride>,
}

/// One `hues:` entry: `<role>: <hue>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HueOverride {
    /// `brass`, `slate`, `moss` or `alert`.
    pub role: String,
    /// Degrees or a named hue.
    pub value: String,
}

/// The indented block under a front matter key being read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FrontBlock {
    None,
    Facts,
    Hues,
}

/// A source split at the end of its front matter.
struct FrontSplit<'a> {
    front: FrontMatter,
    /// Everything after the closing `---`, or the whole text when it has no
    /// front matter.
    body: &'a str,
}

/// Read `text`'s front matter, if it opens with a `---` line, and the body
/// after it. An unclosed block is not front matter.
fn split_front_matter(text: &str) -> FrontSplit<'_> {
    let none = FrontSplit {
        front: FrontMatter::default(),
        body: text,
    };
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return none;
    }
    let mut front = FrontMatter::default();
    let mut at = text.find('\n').map_or(text.len(), |n| n + 1);
    let mut block = FrontBlock::None;
    for line in lines {
        at += line.len();
        let trimmed = line.trim_end();
        if trimmed == "---" {
            return FrontSplit {
                front,
                body: &text[at..],
            };
        }
        let indented = trimmed.starts_with([' ', '\t']);
        if indented && block != FrontBlock::None {
            let item = trimmed.trim_start();
            match block {
                FrontBlock::Facts => {
                    if let Some(fact) = item.strip_prefix("- ") {
                        front.facts.push(unquote(fact).to_owned());
                    }
                }
                FrontBlock::Hues => {
                    if let Some((role, value)) = item.split_once(':') {
                        front.hues.push(HueOverride {
                            role: role.trim().to_owned(),
                            value: unquote(value).to_owned(),
                        });
                    }
                }
                FrontBlock::None => {}
            }
            continue;
        }
        block = FrontBlock::None;
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = unquote(value);
        match key {
            "title" if !value.is_empty() => front.title = Some(value.to_owned()),
            "src" if !value.is_empty() => front.src = Some(value.to_owned()),
            "accent" if !value.is_empty() => front.accent = Some(value.to_owned()),
            "ground" if !value.is_empty() => front.ground = Some(value.to_owned()),
            "facts" => block = FrontBlock::Facts,
            "hues" => block = FrontBlock::Hues,
            _ => {}
        }
    }
    none
}

/// `value` trimmed and out of one pair of matching quotes.
fn unquote(value: &str) -> &str {
    let value = value.trim();
    for q in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(q)
            .and_then(|v| v.strip_suffix(q))
            .filter(|_| value.len() >= 2)
        {
            return inner;
        }
    }
    value
}

/// What a parsed source was parsed from, to tell when it's stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SourceKey {
    /// The record's source event.
    Event(NoteId),
    /// A git note, which has no id of its own: a hash of its text.
    Note(u64),
}

/// The key as a name, for the textures a page's figures are uploaded under:
/// the event's hex id, or `note-<hash>`.
impl fmt::Display for SourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event(id) => f.write_str(&id.hex()),
            Self::Note(hash) => write!(f, "note-{hash:016x}"),
        }
    }
}

/// A source's text and what it is keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceText<'a> {
    pub text: &'a str,
    pub key: SourceKey,
}

impl SourceText<'_> {
    /// Whether the text came from the commit's git note rather than the
    /// record.
    pub(crate) fn is_note(&self) -> bool {
        matches!(self.key, SourceKey::Note(_))
    }
}

/// One explain note's text and the key it's cached by.
struct KeyedNote {
    text: String,
    key: SourceKey,
}

impl KeyedNote {
    fn new(text: Option<String>) -> Option<Self> {
        let text = text?;
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let key = SourceKey::Note(hasher.finish());
        Some(Self { text, key })
    }
}

/// A commit's explain notes, as the review pane reads them: each one hashed
/// into its [`SourceKey`] once, when the load lands, so a frame compares keys
/// without hashing the text again.
#[derive(Default)]
pub(crate) struct ReviewNotes {
    explain: Option<KeyedNote>,
    code: Option<KeyedNote>,
}

impl ReviewNotes {
    pub(crate) fn new(notes: CommitNotes) -> Self {
        Self {
            explain: KeyedNote::new(notes.explain),
            code: KeyedNote::new(notes.code),
        }
    }

    /// The note standing in for `role`'s source, if the commit has one.
    pub(crate) fn get(&self, role: ReviewSourceRole) -> Option<SourceText<'_>> {
        let note = match note_kind(role) {
            NoteKind::Explain => &self.explain,
            NoteKind::Code => &self.code,
        };
        note.as_ref().map(|n| SourceText {
            text: &n.text,
            key: n.key,
        })
    }
}

/// The git note that carries a record source of `role`.
fn note_kind(role: ReviewSourceRole) -> NoteKind {
    match role {
        ReviewSourceRole::Explainer => NoteKind::Explain,
        ReviewSourceRole::Code => NoteKind::Code,
    }
}

/// `role`'s source for the pane: `record`'s own copy first, then the commit's
/// git note out of `notes` (`None` while the commit is still loading).
pub(crate) fn source_text<'a>(
    record: Option<&'a ReviewView>,
    notes: Option<&'a ReviewNotes>,
    role: ReviewSourceRole,
) -> Option<SourceText<'a>> {
    let own = record.and_then(|r| r.source(role)).map(|s| SourceText {
        text: &s.text,
        key: SourceKey::Event(s.id),
    });
    own.or_else(|| notes?.get(role))
}

/// A record's explainer source, ready to draw: its front matter as a header
/// and the rest as Markdown, its figures ([`review_figures`]) drawn. The
/// other explain fences (`flow`, `sequence`, `ba`, `wrong`) render as the
/// code blocks they are to a Markdown reader.
///
/// [`review_figures`]: crate::review_figures
pub(crate) struct ExplainerPage {
    /// What it was parsed from, to tell when it's stale.
    pub source: SourceKey,
    pub front: FrontMatter,
    pub body: Figured,
    /// The palette its figures' `var(--…)` colours come from.
    pub palette: Palette,
    /// Its figures' loads and textures, dropped with the page.
    pub figures: Figures,
}

impl ExplainerPage {
    /// Parse `text`, the source keyed by `key`.
    pub(crate) fn new(text: &str, key: SourceKey) -> Self {
        let FrontSplit { front, body } = split_front_matter(text);
        let mut figure = 0;
        Self {
            source: key,
            body: Figured::parse(&fence_raw_svg(body), &mut figure),
            palette: Palette::of(&front),
            front,
            figures: Figures::new(key),
        }
    }
}

/// `text` with every top-level block that opens with `<svg` wrapped in a
/// ` ```figure ` fence, so it draws as one. The block runs to the first line
/// holding `</svg>`, blank lines and all, as explain reads it (nesting isn't
/// counted); one already inside a fence is left alone.
pub(crate) fn fence_raw_svg(text: &str) -> Cow<'_, str> {
    if !text.contains("<svg") && !text.contains("<SVG") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 32);
    let mut open: Option<Fence> = None;
    let mut in_svg = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if in_svg {
            out.push_str(line);
            if bare.to_ascii_lowercase().contains("</svg>") {
                close_figure(&mut out);
                in_svg = false;
            }
            continue;
        }
        if let Some(fence) = open {
            let closes = fence_of(bare).is_some_and(|f| {
                f.fence.marker == fence.marker && f.fence.len >= fence.len && f.info.is_empty()
            });
            if closes {
                open = None;
            }
            out.push_str(line);
            continue;
        }
        if let Some(f) = fence_of(bare) {
            open = Some(f.fence);
            out.push_str(line);
            continue;
        }
        if opens_svg(bare) {
            out.push_str("```figure\n");
            out.push_str(line);
            if bare.to_ascii_lowercase().contains("</svg>") {
                close_figure(&mut out);
            } else {
                in_svg = true;
            }
            continue;
        }
        out.push_str(line);
    }
    if in_svg {
        close_figure(&mut out);
    }
    Cow::Owned(out)
}

/// Close a figure fence [`fence_raw_svg`] opened, on a line of its own.
fn close_figure(out: &mut String) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
}

/// Whether `line` opens an HTML block with `<svg`: at most three spaces in,
/// then `<svg` followed by whitespace, `>` or the line's end.
fn opens_svg(line: &str) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let Some(tag) = rest.get(..4) else {
        return false;
    };
    if !tag.eq_ignore_ascii_case("<svg") {
        return false;
    }
    rest[4..]
        .chars()
        .next()
        .is_none_or(|c| c == '>' || c.is_whitespace())
}

/// A ` ```hunk ` fence: the hunk it cites.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HunkCite {
    /// The file, by its path before or after the change.
    path: String,
    /// The `@@ … @@` header as written; empty when the fence gave none.
    header: String,
    /// `header`'s ranges.
    at: Option<HunkAt>,
    /// The `match:` line: text one of the hunk's lines holds.
    matches: Option<String>,
}

/// A hunk header's ranges, a missing count read as 1 (as git writes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HunkAt {
    old_start: u32,
    old_len: u32,
    new_start: u32,
    new_len: u32,
}

impl HunkAt {
    /// The ranges of the first `@@ -a[,b] +c[,d] @@` in `s`.
    fn parse(s: &str) -> Option<Self> {
        let rest = &s[s.find("@@ -")? + 4..];
        let (old, rest) = rest.split_once(" +")?;
        let (new, _) = rest.split_once(" @@")?;
        let range = |r: &str| -> Option<(u32, u32)> {
            match r.split_once(',') {
                Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
                None => Some((r.parse().ok()?, 1)),
            }
        };
        let (old_start, old_len) = range(old)?;
        let (new_start, new_len) = range(new)?;
        Some(Self {
            old_start,
            old_len,
            new_start,
            new_len,
        })
    }

    fn of(hunk: &Hunk) -> Self {
        Self {
            old_start: hunk.old_start,
            old_len: hunk.old_len,
            new_start: hunk.new_start,
            new_len: hunk.new_len,
        }
    }
}

impl HunkCite {
    /// The cite of a fence whose info string is `info` (`hunk <path>
    /// [@@ … @@]`) and whose body is `body`. The last `match:` line wins.
    fn parse(info: &str, body: &str) -> Self {
        let rest = info.strip_prefix("hunk").unwrap_or(info).trim();
        let (path, header) = match rest.find("@@") {
            Some(at) => (rest[..at].trim(), rest[at..].trim()),
            None => (rest, ""),
        };
        let matches = body
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("match:"))
            .map(str::trim)
            .next_back()
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        Self {
            path: path.to_owned(),
            header: header.to_owned(),
            at: HunkAt::parse(header),
            matches,
        }
    }
}

/// A hunk of the patch: file `file`, its hunk `hunk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HunkRef {
    pub file: usize,
    pub hunk: usize,
}

/// Does hunk `h` of file `f` hold a line containing `needle`: among its
/// changed lines, or with `context` its context lines too?
fn hunk_has(patch: &GitPatch, f: usize, h: usize, needle: &str, context: bool) -> bool {
    let file = &patch.files()[f];
    file.hunk_lines(&file.hunks[h]).iter().any(|line| {
        let wanted = match line.kind {
            LineKind::Insert | LineKind::Delete => true,
            LineKind::Context => context,
            LineKind::NoNewline => false,
        };
        wanted && patch.text(line.text).trim().contains(needle)
    })
}

/// The hunk `cite` names in `patch`, found as `explain` finds it: the file by
/// its new or old path, then the hunk with the cited header when it still
/// holds the `match:` line; failing that, the hunk whose changed lines (then
/// any lines) hold it, the nearest to the cited header if several do. A file
/// with one hunk needs neither. `None` when nothing fits.
fn find_hunk(patch: &GitPatch, cite: &HunkCite) -> Option<HunkRef> {
    let f = patch
        .files()
        .iter()
        .position(|file| file.new_path == cite.path || file.old_path == cite.path)?;
    let hunks = &patch.files()[f].hunks;
    let at = |hunk| Some(HunkRef { file: f, hunk });
    let hit = cite
        .at
        .and_then(|cited| hunks.iter().position(|h| HunkAt::of(h) == cited));
    if let Some(h) = hit {
        let holds = cite
            .matches
            .as_deref()
            .is_none_or(|m| hunk_has(patch, f, h, m, true));
        if holds {
            return at(h);
        }
    }
    if let Some(needle) = cite.matches.as_deref() {
        for context in [false, true] {
            let near = |h: &usize| {
                cite.at
                    .map_or(0, |a| hunks[*h].new_start.abs_diff(a.new_start))
            };
            // The nearest, the first of equals.
            let found = (0..hunks.len())
                .filter(|&h| hunk_has(patch, f, h, needle, context))
                .min_by_key(near);
            if found.is_some() {
                return found.and_then(at);
            }
        }
    }
    if hit.is_some() {
        // The header fits but the match is nowhere: keep the header's hunk.
        return hit.and_then(at);
    }
    if cite.at.is_none() && cite.matches.is_none() && hunks.len() == 1 {
        return at(0);
    }
    None
}

/// One stretch of a code walk's prose, and where it goes.
pub(crate) struct WalkNote {
    /// The hunk it introduces; `None` for one drawn at the top of the diff:
    /// prose before the first fence or after the last, or a note whose hunk
    /// wasn't found.
    pub at: Option<HunkRef>,
    /// `hunk not found: <path> <header>`, when the fence it came before cites
    /// a hunk the diff hasn't got.
    pub missing: Option<String>,
    /// The prose, as written: the diff's hover and accessible label.
    pub prose: String,
    pub body: Figured,
}

/// A record's code walk, resolved against the diff it is drawn into.
pub(crate) struct CodeWalk {
    /// What it was parsed from, to tell when it's stale.
    pub source: SourceKey,
    /// In walk order.
    pub notes: Vec<WalkNote>,
    /// The palette its figures' `var(--…)` colours come from.
    pub palette: Palette,
    /// Its figures' loads and textures, numbered across all its notes.
    pub figures: Figures,
}

impl CodeWalk {
    /// Parse `text`, the walk keyed by `key`, and find each of its hunks in
    /// `patch`.
    pub(crate) fn resolve(text: &str, key: SourceKey, patch: &GitPatch) -> Self {
        let FrontSplit { front, body } = split_front_matter(text);
        let mut figure = 0;
        let notes = walk_steps(body)
            .into_iter()
            .filter_map(|step| {
                let at = step.cite.as_ref().and_then(|c| find_hunk(patch, c));
                let missing = match (&step.cite, at) {
                    (Some(cite), None) => Some(
                        format!("hunk not found: {} {}", cite.path, cite.header)
                            .trim_end()
                            .to_owned(),
                    ),
                    _ => None,
                };
                let prose = step.prose.trim();
                if prose.is_empty() && missing.is_none() {
                    return None;
                }
                Some(WalkNote {
                    at,
                    missing,
                    body: Figured::parse(&fence_raw_svg(prose), &mut figure),
                    prose: prose.to_owned(),
                })
            })
            .collect();
        Self {
            source: key,
            notes,
            palette: Palette::of(&front),
            figures: Figures::new(key),
        }
    }

    /// The notes drawn at the top of the diff, in walk order.
    pub(crate) fn top(&self) -> impl Iterator<Item = &WalkNote> {
        self.notes.iter().filter(|n| n.at.is_none())
    }
}

/// A hunk fence and the prose after it, up to the next fence: its note. The
/// walk's opening, before any fence, has no fence.
struct WalkStep {
    prose: String,
    cite: Option<HunkCite>,
}

/// A fence's marker character and length.
#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    len: usize,
}

/// A line that opens or closes a fence.
struct FenceLine<'a> {
    fence: Fence,
    /// The info string after the marker, trimmed.
    info: &'a str,
}

/// The fence `line` opens or closes, if it is a fence line.
fn fence_of(line: &str) -> Option<FenceLine<'_>> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let line = &line[indent..];
    let marker = line.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = line.len() - line.trim_start_matches(marker).len();
    (len >= 3).then(|| FenceLine {
        fence: Fence { marker, len },
        info: line[len..].trim(),
    })
}

/// A hunk fence being read: its info string and the body so far.
struct HunkFence {
    info: String,
    body: String,
}

impl HunkFence {
    fn cite(&self) -> HunkCite {
        HunkCite::parse(&self.info, &self.body)
    }
}

/// The fence a walk is inside: a hunk's, read for its cite, or any other,
/// kept in the prose.
struct OpenFence {
    fence: Fence,
    hunk: Option<HunkFence>,
}

/// Split a walk into its steps: its opening, then each hunk fence with the
/// prose after it. Other fences stay in the prose, as code.
fn walk_steps(text: &str) -> Vec<WalkStep> {
    let mut walk = Walk::default();
    let mut open: Option<OpenFence> = None;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if let Some(inside) = &mut open {
            let fence = inside.fence;
            let closes = fence_of(bare).is_some_and(|f| {
                f.fence.marker == fence.marker && f.fence.len >= fence.len && f.info.is_empty()
            });
            match &mut inside.hunk {
                Some(hunk) if closes => walk.cite(hunk.cite()),
                Some(hunk) => hunk.body.push_str(line),
                None => walk.prose.push_str(line),
            }
            if closes {
                open = None;
            }
            continue;
        }
        if let Some(FenceLine { fence, info }) = fence_of(bare) {
            let is_hunk = info == "hunk" || info.starts_with("hunk ");
            let hunk = is_hunk.then(|| HunkFence {
                info: info.to_owned(),
                body: String::new(),
            });
            open = Some(OpenFence { fence, hunk });
            if is_hunk {
                continue;
            }
        }
        walk.prose.push_str(line);
    }
    // An unclosed hunk fence still cites its hunk.
    if let Some(hunk) = open.and_then(|o| o.hunk) {
        walk.cite(hunk.cite());
    }
    let Walk {
        mut steps,
        prose,
        cite,
    } = walk;
    steps.push(WalkStep { prose, cite });
    steps
}

/// [`walk_steps`]' progress: the steps so far, and the fence whose note is
/// being read.
#[derive(Default)]
struct Walk {
    steps: Vec<WalkStep>,
    /// The prose since the last fence: its note, or the opening.
    prose: String,
    /// The last fence; `None` while reading the opening.
    cite: Option<HunkCite>,
}

impl Walk {
    /// A hunk fence citing `cite` closed: the prose read so far belongs to
    /// the fence before it (or is the opening), and `cite` takes the prose
    /// that follows.
    fn cite(&mut self, cite: HunkCite) {
        let prose = std::mem::take(&mut self.prose);
        let before = self.cite.replace(cite);
        self.steps.push(WalkStep {
            prose,
            cite: before,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use headway::event::{ReviewFields, ReviewSourceView};

    const PATCH: &str = "\
diff --git a/src/guard.ts b/src/guard.ts
--- a/src/guard.ts
+++ b/src/guard.ts
@@ -10,3 +10,4 @@ class Guard {
 one
+if (isLocal() && !msAccount)
 two
 three
@@ -40,2 +41,3 @@ fn other
 one more
+five
 six
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
";

    /// The key a walk parsed from a plain string is given.
    const KEY: SourceKey = SourceKey::Note(7);

    fn source(role: ReviewSourceRole, text: &str) -> ReviewSourceView {
        ReviewSourceView {
            id: NoteId::new([7; 32]),
            author: [0; 32],
            role,
            name: "x.code.md".into(),
            text: text.into(),
            created_at: 0,
        }
    }

    fn record(sources: Vec<ReviewSourceView>) -> ReviewView {
        ReviewView {
            id: NoteId::new([1; 32]),
            author: [0; 32],
            created_at: 0,
            fields: ReviewFields::default(),
            comments: Vec::new(),
            sources,
        }
    }

    fn cite(path: &str, header: &str, matches: Option<&str>) -> HunkCite {
        HunkCite {
            path: path.into(),
            header: header.into(),
            at: HunkAt::parse(header),
            matches: matches.map(str::to_owned),
        }
    }

    fn hunk(file: usize, hunk: usize) -> Option<HunkRef> {
        Some(HunkRef { file, hunk })
    }

    /// The front matter's title, src and facts come out of the page; its
    /// other keys are skipped, and the body is what follows it.
    #[test]
    fn front_matter_gives_the_header() {
        let text = "---\ntitle: \"A rate limit\"\nsrc: strfry · RelayServer.h\nfacts:\n  - 3 sites\n  - '1 helper'\naccent: rust\n---\n\nThe lede.\n";
        let split = split_front_matter(text);
        assert_eq!(split.front.title.as_deref(), Some("A rate limit"));
        assert_eq!(split.front.src.as_deref(), Some("strfry · RelayServer.h"));
        assert_eq!(split.front.facts, ["3 sites", "1 helper"]);
        assert_eq!(split.body, "\nThe lede.\n");

        let plain = "# Title\n\nno front matter\n";
        assert_eq!(split_front_matter(plain).body, plain);
        let unclosed = "---\ntitle: x\n";
        assert_eq!(split_front_matter(unclosed).body, unclosed);
        assert_eq!(split_front_matter(unclosed).front, FrontMatter::default());
    }

    /// `accent`, `ground` and `hues:` are kept for the figures' palette.
    #[test]
    fn front_matter_keeps_the_palette_keys() {
        let text = "---\naccent: rust\nground: '92'\nhues:\n  moss: 150\n  alert: red\nfacts:\n  - 1 fact\n---\nBody.\n";
        let front = split_front_matter(text).front;
        assert_eq!(front.accent.as_deref(), Some("rust"));
        assert_eq!(front.ground.as_deref(), Some("92"));
        assert_eq!(
            front.hues,
            [
                HueOverride {
                    role: "moss".into(),
                    value: "150".into()
                },
                HueOverride {
                    role: "alert".into(),
                    value: "red".into()
                },
            ]
        );
        assert_eq!(front.facts, ["1 fact"]);
    }

    /// A top-level `<svg` block is fenced as a figure through its `</svg>`,
    /// blank lines and all; one inside a fence, an indented one, or a
    /// `<svgfoo>` is left alone.
    #[test]
    fn fence_raw_svg_keeps_blank_lines() {
        let text = "Intro.\n<svg viewBox=\"0 0 1 1\">\n  <g/>\n\n  <g/>\n</svg>\nAfter.\n\n```html\n<svg>\n```\n    <svg>\n<svgfoo>\n<SVG></SVG>\n";
        assert_eq!(
            fence_raw_svg(text),
            "Intro.\n```figure\n<svg viewBox=\"0 0 1 1\">\n  <g/>\n\n  <g/>\n</svg>\n```\nAfter.\n\n```html\n<svg>\n```\n    <svg>\n<svgfoo>\n```figure\n<SVG></SVG>\n```\n"
        );
        assert!(matches!(fence_raw_svg("no drawings"), Cow::Borrowed(_)));
        // An unclosed one runs to the end, and is closed there.
        assert_eq!(
            fence_raw_svg("<svg>\n<g/>"),
            "```figure\n<svg>\n<g/>\n```\n"
        );
    }

    /// A fence's info string gives the path and header, its body the last
    /// `match:` line.
    #[test]
    fn a_hunk_fence_parses_to_its_cite() {
        let c = HunkCite::parse(
            "hunk src/guard.ts @@ -10,3 +10,4 @@",
            "match: nope\n  match: if (isLocal())\n",
        );
        assert_eq!(c.path, "src/guard.ts");
        assert_eq!(c.header, "@@ -10,3 +10,4 @@");
        assert_eq!(
            c.at,
            Some(HunkAt {
                old_start: 10,
                old_len: 3,
                new_start: 10,
                new_len: 4
            })
        );
        assert_eq!(c.matches.as_deref(), Some("if (isLocal())"));
        let bare = HunkCite::parse("hunk README.md", "");
        assert_eq!(
            (bare.path.as_str(), bare.at, bare.matches),
            ("README.md", None, None)
        );
        assert_eq!(HunkAt::parse("@@ -1 +1 @@").map(|a| a.old_len), Some(1));
    }

    /// Found by header; by `match:` once a rebase moved the header (the
    /// nearest when several hold it, changed lines before context); a lone hunk with nothing cited; and
    /// nothing for a file or hunk the diff hasn't got.
    #[test]
    fn hunks_are_found_as_explain_finds_them() {
        let patch = GitPatch::parse(PATCH);
        let find = |c: HunkCite| find_hunk(&patch, &c);
        assert_eq!(
            find(cite("src/guard.ts", "@@ -40,2 +41,3 @@", None)),
            hunk(0, 1)
        );
        // Rebased: the header moved, the match finds it.
        assert_eq!(
            find(cite("src/guard.ts", "@@ -8,3 +8,4 @@", Some("isLocal()"))),
            hunk(0, 0)
        );
        // The header names hunk 1, but the match is only in hunk 0.
        assert_eq!(
            find(cite("src/guard.ts", "@@ -40,2 +41,3 @@", Some("msAccount"))),
            hunk(0, 0)
        );
        // A match only a context line holds.
        assert_eq!(
            find(cite("src/guard.ts", "@@ -39,2 +39,3 @@", Some("six"))),
            hunk(0, 1)
        );
        // No hunk has the cited header; hunk 1 has an "o" in its context,
        // but hunk 0 has one in a changed line, which wins.
        assert_eq!(
            find(cite("src/guard.ts", "@@ -38,2 +38,3 @@", Some("o"))),
            hunk(0, 0)
        );
        // Both hold it in a context line: the nearest the cited header wins.
        assert_eq!(
            find(cite("src/guard.ts", "@@ -9,3 +9,4 @@", Some("one"))),
            hunk(0, 0)
        );
        assert_eq!(
            find(cite("src/guard.ts", "@@ -45,2 +45,3 @@", Some("one"))),
            hunk(0, 1)
        );
        assert_eq!(find(cite("README.md", "", None)), hunk(1, 0));
        assert_eq!(find(cite("src/guard.ts", "", None)), None);
        assert_eq!(find(cite("gone.rs", "@@ -1 +1 @@", None)), None);
        assert_eq!(
            find(cite("src/guard.ts", "@@ -99 +99 @@", Some("absent"))),
            None
        );
    }

    /// A fence's note is the prose after it, up to the next fence, code
    /// blocks and all; the fence text is dropped. Prose before the first
    /// fence is the opening, at the top. A fence with no prose after it adds
    /// no note, and one that finds nothing puts its note at the top with why.
    #[test]
    fn a_walk_threads_the_prose_after_each_fence_onto_its_hunk() {
        let text = "\
---
title: walk
---
Opening.

Second opening paragraph.

```hunk src/guard.ts @@ -40,2 +41,3 @@
```

About hunk 1:

```rust
let x = 1;
```

```hunk src/guard.ts @@ -10,3 +10,4 @@
match: isLocal()
```
About the guard.
```hunk README.md
```
```hunk gone.rs @@ -1 +1 @@
```

Missing one.
";
        let walk = CodeWalk::resolve(text, KEY, &GitPatch::parse(PATCH));
        assert_eq!(walk.source, KEY);
        let shown: Vec<_> = walk
            .notes
            .iter()
            .map(|n| (n.at, n.missing.as_deref(), n.prose.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (None, None, "Opening.\n\nSecond opening paragraph."),
                (
                    hunk(0, 1),
                    None,
                    "About hunk 1:\n\n```rust\nlet x = 1;\n```"
                ),
                (hunk(0, 0), None, "About the guard."),
                (
                    None,
                    Some("hunk not found: gone.rs @@ -1 +1 @@"),
                    "Missing one."
                ),
            ]
        );
        let top: Vec<_> = walk.top().map(|n| n.prose.as_str()).collect();
        assert_eq!(
            top,
            ["Opening.\n\nSecond opening paragraph.", "Missing one."]
        );
        assert!(walk.notes.iter().all(|n| !n.prose.contains("match:")));
    }

    /// A walk with no fences is all opening; one ending in an unclosed fence
    /// still cites its hunk.
    #[test]
    fn a_walk_without_fences_is_its_opening() {
        let steps = walk_steps("Just prose.\n");
        assert_eq!(steps.len(), 1);
        assert!(steps[0].cite.is_none());
        let unclosed = walk_steps("Open.\n```hunk a.rs\nmatch: x\n");
        assert_eq!(unclosed.len(), 2);
        assert_eq!(
            unclosed[1].cite.as_ref().map(|c| c.path.as_str()),
            Some("a.rs")
        );
        assert_eq!(unclosed[1].prose, "");
    }

    /// A record's own source wins; a role it lacks falls back to the commit's
    /// note, keyed by its text's hash; with neither (or the notes still
    /// loading) there is none.
    #[test]
    fn the_record_comes_first_then_the_note() {
        let notes = ReviewNotes::new(CommitNotes {
            explain: Some("# the note's page".into()),
            code: Some("the note's walk".into()),
        });
        let own = record(vec![source(ReviewSourceRole::Explainer, "# own page")]);
        let page = source_text(Some(&own), Some(&notes), ReviewSourceRole::Explainer).unwrap();
        assert_eq!(page.text, "# own page");
        assert_eq!(page.key, SourceKey::Event(NoteId::new([7; 32])));
        assert!(!page.is_note());

        let walk = source_text(Some(&own), Some(&notes), ReviewSourceRole::Code).unwrap();
        assert_eq!(walk.text, "the note's walk");
        assert!(walk.is_note());
        let again = ReviewNotes::new(CommitNotes {
            explain: None,
            code: Some("the note's walk".into()),
        });
        assert_eq!(
            again.get(ReviewSourceRole::Code).map(|s| s.key),
            Some(walk.key)
        );
        assert_eq!(again.get(ReviewSourceRole::Explainer), None);

        assert_eq!(source_text(Some(&own), None, ReviewSourceRole::Code), None);
        let bare = record(Vec::new());
        assert_eq!(
            source_text(
                Some(&bare),
                Some(&ReviewNotes::default()),
                ReviewSourceRole::Code
            ),
            None
        );
    }

    /// A note's page parses like a record's; a squashed note's second page
    /// stays in the body, its front matter as text.
    #[test]
    fn a_page_parses_from_a_plain_string() {
        let text = "---\ntitle: first\n---\nOne.\n\n---\ntitle: second\n---\nTwo.\n";
        let page = ExplainerPage::new(text, KEY);
        assert_eq!(page.source, KEY);
        assert_eq!(page.front.title.as_deref(), Some("first"));
        let body = split_front_matter(text).body;
        assert!(body.contains("title: second") && body.contains("Two."));
    }

    /// A note's key names its figures' textures apart from any event's.
    #[test]
    fn a_note_key_names_textures_apart_from_events() {
        assert_eq!(SourceKey::Note(0xab).to_string(), "note-00000000000000ab");
        let id = NoteId::new([1; 32]);
        assert_eq!(SourceKey::Event(id).to_string(), id.hex());
    }
}
