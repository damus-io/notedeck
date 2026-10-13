//! The Cmd-K bar's universal search: what each app finds for the query,
//! listed beside the tools.
//!
//! [`Chrome::search_if_due`] runs after the bar draws, so it sees the query
//! the field just took. When the query changed it asks every provider (see
//! [`notedeck::search`] for their contract) and hands the bar their hits; a
//! frame whose query didn't change asks nothing, unless a provider's answer
//! was pending and its [`REFINE_INTERVAL`] is up, when only that provider is
//! asked again. An empty query, or the bar asking for a tool's argument,
//! runs no search: the query is a value then, not a search.
//!
//! The providers are the chrome's own two, then each running app's
//! [`App::search`](notedeck::App::search), the active app first:
//!
//! - A query that is one whole reference (`headway:board/word-id`,
//!   `nostr:note1…`) that resolves is an "Open" row for it.
//! - A query matching an app's name is a row that switches to that app.
//!
//! An app's hits are listed under its name, each opening its
//! [`uri`](notedeck::SearchHit::uri) as [`AppAction::Open`](notedeck::AppAction)
//! does.

use std::borrow::Cow;

use notedeck::fuzzy::fuzzy_match;
use notedeck::search::{HITS_PER_PROVIDER, REFINE_INTERVAL};
use notedeck::{
    tr, App, AppContext, AppId, CommandAction, CommandItem, OpenUri, ReferenceParserRegistry,
    SearchHit, SearchHits, SearchQuery,
};

use super::actions::resolve_reference;
use super::sidebar::app_label;
use super::Chrome;

/// Who answered a query.
#[derive(Clone, Copy, PartialEq)]
enum Provider {
    /// The chrome itself: references and app names.
    Chrome,
    /// The app in this slot of the roster.
    App(usize),
}

/// One provider's answer to the query, as the bar's rows.
struct Answer {
    /// Who gave it.
    provider: Provider,
    /// Its hits, as rows.
    rows: Vec<CommandItem>,
    /// Whether more is coming: the provider is asked again.
    pending: bool,
}

/// The search behind the open bar: the query it answers and each
/// provider's answer.
pub(super) struct SearchState {
    /// The query, trimmed, that `answers` answer.
    query: String,
    /// Each provider's answer, the chrome's first, then the apps' in the
    /// order they were asked.
    answers: Vec<Answer>,
    /// When to ask the pending providers again, in egui input time.
    refine_at: Option<f64>,
    /// The buffer each app answers into, reused across them.
    hits: SearchHits,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            answers: Vec::new(),
            refine_at: None,
            hits: SearchHits::new(HITS_PER_PROVIDER),
        }
    }
}

impl Chrome {
    /// Search for the open bar's query if it changed since the last search,
    /// or ask a pending provider again if its time is up (see the module
    /// docs). Does nothing, and allocates nothing, on any other frame.
    #[profiling::function]
    pub(super) fn search_if_due(&mut self, ctx: &mut AppContext, egui_ctx: &egui::Context) {
        let bar = &self.command_bar;
        if !bar.is_open() || bar.prompt().is_some() {
            return;
        }
        let now = egui_ctx.input(|i| i.time);
        let query = bar.query().trim();
        if query != bar.search.query {
            let query = query.to_owned();
            self.search(ctx, query);
        } else if bar.search.refine_at.is_some_and(|at| now >= at) {
            self.refine(ctx);
        } else {
            return;
        }

        let search = &mut self.command_bar.search;
        let pending = search.answers.iter().any(|answer| answer.pending);
        search.refine_at = pending.then(|| now + REFINE_INTERVAL.as_secs_f64());
        if pending {
            egui_ctx.request_repaint_after(REFINE_INTERVAL);
        }
        let rows: Vec<CommandItem> = search
            .answers
            .iter()
            .flat_map(|answer| answer.rows.iter().cloned())
            .collect();
        // Unchanged hits are left alone: setting them refilters, which
        // would move the selection back to the top under a key that moved
        // it, each time a pending provider is asked again.
        if rows == self.command_bar.hits() {
            return;
        }
        self.command_bar.set_hits(rows);
        // The bar drew this frame with the last query's hits; draw the new.
        egui_ctx.request_repaint();
    }

    /// Ask every provider about `query`, replacing the last query's answers.
    fn search(&mut self, ctx: &mut AppContext, query: String) {
        let mut answers = std::mem::take(&mut self.command_bar.search.answers);
        answers.clear();
        if !query.is_empty() {
            answers.push(Answer {
                provider: Provider::Chrome,
                rows: self.chrome_rows(ctx, &query),
                pending: false,
            });
            let active = self.active as usize;
            let others = (0..self.apps.len()).filter(|&slot| slot != active);
            for slot in std::iter::once(active).chain(others) {
                if slot < self.apps.len() && self.is_running(slot) {
                    answers.push(self.ask_app(ctx, slot, &query));
                }
            }
        }
        let search = &mut self.command_bar.search;
        search.query = query;
        search.answers = answers;
    }

    /// Ask the providers whose answer was pending again, keeping the rest.
    fn refine(&mut self, ctx: &mut AppContext) {
        let mut answers = std::mem::take(&mut self.command_bar.search.answers);
        let query = std::mem::take(&mut self.command_bar.search.query);
        for answer in answers.iter_mut().filter(|answer| answer.pending) {
            if let Provider::App(slot) = answer.provider {
                *answer = self.ask_app(ctx, slot, &query);
            }
        }
        let search = &mut self.command_bar.search;
        search.query = query;
        search.answers = answers;
    }

    /// The app in `slot`'s answer to `query`, its hits listed under its
    /// name.
    fn ask_app(&mut self, ctx: &mut AppContext, slot: usize, query: &str) -> Answer {
        let mut hits = std::mem::replace(&mut self.command_bar.search.hits, SearchHits::new(0));
        hits.clear();
        self.apps[slot].search(ctx, &SearchQuery { text: query }, &mut hits);
        let section = app_label(ctx.i18n, &self.apps[slot]);
        let rows = hits
            .hits()
            .iter()
            .map(|hit| hit_row(hit, Cow::Owned(section.clone())))
            .collect();
        let answer = Answer {
            provider: Provider::App(slot),
            rows,
            pending: hits.is_pending(),
        };
        self.command_bar.search.hits = hits;
        answer
    }

    /// The chrome's own rows for `query`: an "Open" row when it is one
    /// whole reference that resolves, then a row for each other app whose
    /// name it matches.
    fn chrome_rows(&self, ctx: &mut AppContext, query: &str) -> Vec<CommandItem> {
        let mut rows = Vec::new();
        if let Some(open) = whole_reference(&ctx.registries.reference_parsers, query) {
            if resolve_reference(ctx, &open.reference).is_some() {
                let subtitle = tr!(
                    ctx.i18n,
                    "Open",
                    "Command bar: subtitle of the row that opens the reference typed into it"
                );
                // Titled with the query itself, so it matches it best of
                // anything and is listed first.
                let row =
                    CommandItem::new("", query, CommandAction::Open(open)).subtitled(subtitle);
                rows.push(row.found(0));
            }
        }

        let section = tr!(
            ctx.i18n,
            "Apps",
            "Command bar section listing the apps whose name matches the query"
        );
        let subtitle = tr!(
            ctx.i18n,
            "Switch to app",
            "Command bar: subtitle of a row that switches to the app it names"
        );
        let mut scratch = Vec::new();
        for (slot, app) in self.apps.iter().enumerate() {
            if slot == self.active as usize {
                continue;
            }
            let label = app_label(ctx.i18n, app);
            let Some(score) = fuzzy_match(query, &label, &mut scratch) else {
                continue;
            };
            let row = CommandItem::new(
                Cow::Owned(section.clone()),
                label,
                CommandAction::SwitchApp(AppId(slot)),
            );
            rows.push(row.subtitled(subtitle.clone()).found(score));
        }
        rows
    }
}

/// `query` as an open of the one reference it is, `?msg=` and all, when a
/// parser in `parsers` recognizes the whole of it before any `?`: prose that
/// merely holds a reference isn't one.
fn whole_reference(parsers: &ReferenceParserRegistry, query: &str) -> Option<OpenUri> {
    let open = OpenUri::parse(query)?;
    let found = parsers.find_next(&open.reference)?;
    (found.range == (0..open.reference.len())).then_some(open)
}

/// The bar's row for `hit`, under `section`: its title, its kind and
/// subtitle after it, opening its reference.
fn hit_row(hit: &SearchHit, section: Cow<'static, str>) -> CommandItem {
    let row = CommandItem::new(
        section,
        hit.title.as_str(),
        CommandAction::Open(hit.uri.clone()),
    )
    .found(hit.score);
    let subtitle = match (&hit.kind, &hit.subtitle) {
        (Some(kind), Some(subtitle)) => Some(format!("{kind} · {subtitle}")),
        (Some(kind), None) => Some(kind.clone()),
        (None, Some(subtitle)) => Some(subtitle.clone()),
        (None, None) => None,
    };
    match subtitle {
        Some(subtitle) => row.subtitled(subtitle),
        None => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notedeck::{ReferenceParser, ReferenceResolveCtx, ResolvedRef};
    use std::ops::Range;

    /// Recognizes `stub:` and the lowercase letters and dashes after it.
    struct Stub;

    impl ReferenceParser for Stub {
        fn id(&self) -> &'static str {
            "stub"
        }

        fn find(&self, text: &str) -> Option<Range<usize>> {
            let start = text.find("stub:")?;
            let rest = &text[start + 5..];
            let len = rest
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(rest.len());
            (len > 0).then_some(start..start + 5 + len)
        }

        fn resolve(&self, _matched: &str, _ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
            None
        }
    }

    fn parsers() -> ReferenceParserRegistry {
        let mut parsers = ReferenceParserRegistry::default();
        parsers.register(Box::new(Stub));
        parsers
    }

    #[test]
    fn a_whole_reference_is_one_open() {
        let parsers = parsers();
        let open = |q| whole_reference(&parsers, q);
        assert_eq!(open("stub:a-b-c"), Some(OpenUri::new("stub:a-b-c")));
        assert_eq!(
            open("nostr:note1abc"),
            Some(OpenUri::new("nostr:note1abc")),
            "the built-in nostr parser's too"
        );
        let with_msg = open("stub:a-b-c?msg=hi").expect("a reference with a message");
        assert_eq!(with_msg.reference, "stub:a-b-c");
        assert_eq!(with_msg.msg.as_deref(), Some("hi"));
    }

    #[test]
    fn prose_holding_a_reference_is_not_one() {
        let parsers = parsers();
        let open = |q| whole_reference(&parsers, q);
        assert_eq!(open("see stub:a-b-c"), None);
        assert_eq!(open("stub:a-b-c and more"), None);
        assert_eq!(open("stub:"), None, "no token");
        assert_eq!(open("headway"), None);
        assert_eq!(open(""), None);
    }

    #[test]
    fn a_hits_kind_and_subtitle_read_after_its_title() {
        let uri = OpenUri::new("stub:a-b-c");
        let hit = SearchHit::new("Fix the bar", uri.clone(), 7);
        let row = hit_row(&hit.clone().kind("Card").subtitled("Todo"), "Fake".into());
        assert_eq!(row.title, "Fix the bar");
        assert_eq!(row.subtitle.as_deref(), Some("Card · Todo"));
        assert_eq!(row.found, Some(7));
        assert_eq!(row.action, CommandAction::Open(uri));
        assert_eq!(
            hit_row(&hit.clone().kind("Card"), "".into())
                .subtitle
                .as_deref(),
            Some("Card")
        );
        assert_eq!(hit_row(&hit, "".into()).subtitle, None);
    }
}
