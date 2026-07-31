//! The front half of inline references: recognizing a reference inside a run of
//! text and resolving it to a nostr entity.
//!
//! A surface (a notebook note, a chat message, …) can mention an entity inline —
//! `nostr:nevent1…`, or a headway card as a bare `board#maple-river-canyon`.
//! There is **no shared syntax**: a nostr reference carries a `nostr:` scheme, a
//! headway reference is a bare `slug#word-word-word` with no prefix at all, and a
//! future scheme might use `@handle` or `#tag`. So a [`ReferenceParser`] owns its
//! *whole* grammar — it [`find`](ReferenceParser::find)s its own references in
//! text and [`resolve`](ReferenceParser::resolve)s a match to a concrete note
//! ([`ResolvedRef`]). The *back* half — the
//! [`KindRendererRegistry`](crate::KindRendererRegistry) — then draws that note
//! inline. The two halves never reference each other: an app registers a parser
//! for its references and a renderer for its kind, and the browser mediates.
//!
//! `nostr:` is the built-in parser (see [`NostrRefParser`]), reproducing the
//! prior hardcoded bech32 behaviour; apps add their own via
//! [`App::reference_parsers`](crate::App::reference_parsers). Parsers are
//! registered at startup for *all* apps — like inline
//! [`kind_renderers`](crate::App::kind_renderers) and unlike agent
//! [`tools`](crate::App::tools) — so a reference resolves even for an app the
//! user never opened. A parser resolves against `ndb` (holding its own fold
//! cache if resolution is expensive), not against a live app instance, which is
//! what makes that possible.

use std::collections::HashMap;
use std::ops::Range;

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};

/// The concrete nostr entity a [`ReferenceParser`] resolved a matched reference
/// to.
///
/// Carries the resolved note; the back half fetches it by id and hands it to the
/// [`KindRenderer`](crate::KindRenderer) for its kind. A thin newtype rather than
/// a bare [`NoteId`] so it can grow (e.g. a render hint or an addressable
/// coordinate) without churning every parser signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRef {
    /// The resolved note; drawn via the [`KindRenderer`](crate::KindRenderer) for
    /// its kind.
    pub note_id: NoteId,
}

impl ResolvedRef {
    /// A reference that resolved to the note `note_id`.
    pub fn note(note_id: NoteId) -> Self {
        Self { note_id }
    }
}

/// The read-only context a [`ReferenceParser`] resolves a match against.
///
/// Grouped into a struct (rather than loose args) so a parser signature is
/// stable as resolution grows more inputs. Everything here is shared/read-only:
/// resolution must not mutate host state.
pub struct ReferenceResolveCtx<'a> {
    /// The database to resolve against.
    pub ndb: &'a Ndb,
    /// A live read transaction for `ndb`, so all references in one body share a
    /// single transaction.
    pub txn: &'a Transaction,
    /// The selected account, when there is one. Some parsers (e.g. `headway`)
    /// carry no author in the reference and resolve relative to the current
    /// account.
    pub selected_account: Option<Pubkey>,
}

/// Recognizes and resolves one *kind* of inline reference — its whole grammar,
/// no shared delimiter.
///
/// An app registers one parser per reference syntax it owns (see
/// [`App::reference_parsers`](crate::App::reference_parsers)); the browser asks
/// every registered parser to [`find`](Self::find) its next match in a run of
/// text, takes the leftmost, and resolves it with [`resolve`](Self::resolve).
pub trait ReferenceParser {
    /// A stable identifier for this parser — the registry key, and the handle a
    /// per-parser default is stored under in settings. Must be unique across
    /// registered parsers; `"nostr"` is reserved for the [built-in](NostrRefParser).
    ///
    /// This is *not* a syntax marker: unlike a URI scheme it never has to appear
    /// in the referenced text (a headway reference is a bare
    /// `board#word-word-word`). The grammar lives entirely in [`find`](Self::find).
    fn id(&self) -> &'static str;

    /// Find the next reference this parser recognizes in `text`, as a byte range
    /// into `text`, or `None` if `text` holds none.
    ///
    /// The parser owns its entire grammar — there is no shared `scheme:` prefix,
    /// so `nostr` matches its scheme plus a bech32 run while `headway` matches a
    /// bare `slug#word-word-word`. The returned range must be non-empty and fall
    /// on char boundaries.
    ///
    /// `find` may match **loosely** — a cheap syntactic filter — because a match
    /// that [`resolve`](Self::resolve) then rejects is drawn as ordinary text, so
    /// a false positive is invisible rather than a broken widget. Runs inline
    /// while drawing a frame: keep it linear and allocation-free.
    fn find(&self, text: &str) -> Option<Range<usize>>;

    /// Resolve `matched` — the exact substring [`find`](Self::find) delimited — to
    /// a concrete nostr entity, or `None` if it doesn't resolve (an unparseable
    /// match, or an entity not in the local db yet), in which case it renders as
    /// plain text.
    ///
    /// Must be non-blocking — it runs inline while drawing a frame. A parser that
    /// needs an expensive computation (e.g. folding a board) should cache it
    /// behind interior mutability (`Rc<RefCell<…>>`), the same way
    /// [`KindRenderer`](crate::KindRenderer) impls do.
    fn resolve(&self, matched: &str, ctx: &ReferenceResolveCtx) -> Option<ResolvedRef>;
}

/// App-registered [`ReferenceParser`]s indexed by [id](ReferenceParser::id).
///
/// Lives in [`AppRegistries`](crate::AppRegistries). Always contains the built-in
/// [`nostr`](NostrRefParser) parser (seeded by [`Default`]); apps add more at
/// startup. Later registration of an id replaces an earlier one, so an app must
/// not re-register the reserved `nostr` id.
pub struct ReferenceParserRegistry {
    by_id: HashMap<&'static str, Box<dyn ReferenceParser>>,
}

impl Default for ReferenceParserRegistry {
    /// A registry seeded with only the built-in [`nostr`](NostrRefParser) parser.
    fn default() -> Self {
        let mut reg = Self {
            by_id: HashMap::new(),
        };
        reg.register(Box::new(NostrRefParser));
        reg
    }
}

impl ReferenceParserRegistry {
    /// Register a parser under its [id](ReferenceParser::id), replacing any parser
    /// already present for that id.
    pub fn register(&mut self, parser: Box<dyn ReferenceParser>) {
        self.by_id.insert(parser.id(), parser);
    }

    /// The parser registered under `id`, if any.
    pub fn get(&self, id: &str) -> Option<&dyn ReferenceParser> {
        self.by_id.get(id).map(|b| b.as_ref())
    }

    /// Every registered parser, in arbitrary order. The scanner asks each to
    /// [`find`](ReferenceParser::find) its next match and keeps the leftmost.
    pub fn iter(&self) -> impl Iterator<Item = &dyn ReferenceParser> + '_ {
        self.by_id.values().map(|b| b.as_ref())
    }
}

/// The built-in `nostr:` reference parser: matches a `nostr:` scheme followed by
/// a bech32 entity (`nevent1…`/`note1…`/`naddr1…`) and resolves it via
/// [`resolve_ref`](crate::resolve_ref), reproducing the behaviour the markdown
/// scanner hardcoded before parsers existed. A zero-sized, stateless parser.
pub struct NostrRefParser;

impl NostrRefParser {
    /// The scheme prefix every nostr reference carries.
    const PREFIX: &'static str = "nostr:";
}

impl ReferenceParser for NostrRefParser {
    fn id(&self) -> &'static str {
        "nostr"
    }

    fn find(&self, text: &str) -> Option<Range<usize>> {
        let mut from = 0;
        loop {
            let start = from + text[from..].find(Self::PREFIX)?;
            let token_start = start + Self::PREFIX.len();
            // A bech32 token is a run of lowercase letters/digits (hrp + data) —
            // exactly the delimiter the scanner used for `nostr:` before.
            let rest = &text[token_start..];
            let len = rest
                .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
                .unwrap_or(rest.len());
            if len > 0 {
                return Some(start..token_start + len);
            }
            // A bare `nostr:` with no token: skip it and keep scanning.
            from = token_start;
        }
    }

    fn resolve(&self, matched: &str, ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
        let bech = matched.strip_prefix(Self::PREFIX)?;
        let note = crate::resolve_ref(ctx.ndb, ctx.txn, bech)?;
        Some(ResolvedRef::note(NoteId::new(*note.id())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub parser matching a bare `@handle` — a reference with *no* scheme
    /// prefix, exercising the open-ended [`find`](ReferenceParser::find) contract.
    struct StubParser;
    impl ReferenceParser for StubParser {
        fn id(&self) -> &'static str {
            "stub"
        }
        fn find(&self, text: &str) -> Option<Range<usize>> {
            let at = text.find('@')?;
            let rest = &text[at + 1..];
            let len = rest
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(rest.len());
            (len > 0).then_some(at..at + 1 + len)
        }
        fn resolve(&self, _matched: &str, _ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
            None
        }
    }

    #[test]
    fn default_registry_has_builtin_nostr() {
        let reg = ReferenceParserRegistry::default();
        assert!(reg.get("nostr").is_some());
        assert_eq!(reg.get("nostr").unwrap().id(), "nostr");
        assert!(reg.get("stub").is_none());
    }

    #[test]
    fn register_adds_id_alongside_builtin() {
        let mut reg = ReferenceParserRegistry::default();
        reg.register(Box::new(StubParser));
        assert!(reg.get("nostr").is_some());
        assert!(reg.get("stub").is_some());
        let mut ids: Vec<_> = reg.iter().map(|p| p.id()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["nostr", "stub"]);
    }

    #[test]
    fn nostr_find_matches_bech32_and_skips_bare() {
        let p = NostrRefParser;
        // Matches the scheme plus its bech32 token, stopping at the space.
        let s = "see nostr:nevent1abc def";
        assert_eq!(&s[p.find(s).unwrap()], "nostr:nevent1abc");
        // Uppercase is outside the bech32 class, ending the token.
        let s = "note1: nostr:note1xyzABC";
        assert_eq!(&s[p.find(s).unwrap()], "nostr:note1xyz");
        // A bare `nostr:` with no token is skipped, not matched.
        assert!(p.find("nostr: trailing").is_none());
        // …and a later valid reference past a bare one is still found.
        let s = "nostr: then nostr:note1ok";
        assert_eq!(&s[p.find(s).unwrap()], "nostr:note1ok");
        // Prose with no reference yields nothing.
        assert!(p.find("just prose").is_none());
    }

    #[test]
    fn stub_find_uses_its_own_bare_grammar() {
        let p = StubParser;
        let s = "hi @alice, hello";
        assert_eq!(&s[p.find(s).unwrap()], "@alice");
        // A bare `@` with no handle is not a match.
        assert!(p.find("mail me @ home").is_none());
    }
}
