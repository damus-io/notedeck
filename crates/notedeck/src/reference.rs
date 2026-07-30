//! The front half of inline references: turning a scheme-prefixed text token
//! into a resolvable nostr entity.
//!
//! A surface (a notebook note, a chat message, …) can mention an entity by a
//! short textual reference like `nostr:nevent1…` or `headway:my-board#maple-river-canyon`.
//! The universal shape is `scheme:token`: the part before the first `:` names a
//! [`ReferenceParser`] registered for that scheme, and the parser turns the
//! `token` into a concrete note ([`ResolvedRef`]). The *back* half — the
//! [`KindRendererRegistry`](crate::KindRendererRegistry) — then draws that note
//! inline. The two halves never reference each other: an app registers a parser
//! for its scheme and a renderer for its kind, and the browser mediates.
//!
//! `nostr:` is the built-in scheme (see [`NostrRefParser`]), reproducing the
//! prior hardcoded bech32 behaviour; apps add their own via
//! [`App::reference_parsers`](crate::App::reference_parsers). Parsers are
//! registered at startup for *all* apps — like inline
//! [`kind_renderers`](crate::App::kind_renderers) and unlike agent
//! [`tools`](crate::App::tools) — so a reference resolves even for an app the
//! user never opened. A parser resolves against `ndb` (holding its own fold
//! cache if resolution is expensive), not against a live app instance, which is
//! what makes that possible.

use std::collections::HashMap;

use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Transaction};

/// The concrete nostr entity a [`ReferenceParser`] resolved a text token to.
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

/// The read-only context a [`ReferenceParser`] resolves a token against.
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
    /// The selected account, when there is one. Some schemes (e.g. `headway`)
    /// carry no author in the token and resolve relative to the current account.
    pub selected_account: Option<Pubkey>,
}

/// Turns a scheme-prefixed text reference (`scheme:token`) into a resolvable
/// nostr entity.
///
/// An app registers one parser per scheme it owns (see
/// [`App::reference_parsers`](crate::App::reference_parsers)); the browser scans
/// text for a registered `scheme:` prefix, carves the token out with
/// [`token_len`](Self::token_len), and resolves it with [`resolve`](Self::resolve).
pub trait ReferenceParser {
    /// The URI-like scheme this parser owns — the text before the `:`, e.g.
    /// `"nostr"` or `"headway"`. Must be unique across registered parsers;
    /// `"nostr"` is reserved for the [built-in](NostrRefParser).
    fn scheme(&self) -> &'static str;

    /// Length in bytes of the reference token that begins `rest` — the text
    /// immediately after this parser's `scheme:` prefix — or `0` if no valid
    /// token starts there (so the scanner leaves a bare `scheme:` as plain text).
    ///
    /// This replaces the single hardcoded bech32 char-class the scanner used for
    /// `nostr:`, letting each scheme define its own token grammar.
    fn token_len(&self, rest: &str) -> usize;

    /// Resolve `token` (the text after the `scheme:` prefix) to a concrete nostr
    /// entity, or `None` if it can't be resolved (unparseable token, or the
    /// entity isn't in the local db yet).
    ///
    /// Must be non-blocking — it runs inline while drawing a frame. A parser that
    /// needs an expensive computation (e.g. folding a board) should cache it
    /// behind interior mutability (`Rc<RefCell<…>>`), the same way
    /// [`KindRenderer`](crate::KindRenderer) impls do.
    fn resolve(&self, token: &str, ctx: &ReferenceResolveCtx) -> Option<ResolvedRef>;
}

/// App-registered [`ReferenceParser`]s indexed by [scheme](ReferenceParser::scheme).
///
/// Lives in [`AppRegistries`](crate::AppRegistries). Always contains the built-in
/// [`nostr`](NostrRefParser) parser (seeded by [`Default`]); apps add more at
/// startup. Later registration of a scheme replaces an earlier one, so an app
/// must not re-register the reserved `nostr` scheme.
pub struct ReferenceParserRegistry {
    by_scheme: HashMap<&'static str, Box<dyn ReferenceParser>>,
}

impl Default for ReferenceParserRegistry {
    /// A registry seeded with only the built-in [`nostr`](NostrRefParser) parser.
    fn default() -> Self {
        let mut reg = Self {
            by_scheme: HashMap::new(),
        };
        reg.register(Box::new(NostrRefParser));
        reg
    }
}

impl ReferenceParserRegistry {
    /// Register a parser under its scheme, replacing any parser already present
    /// for that scheme.
    pub fn register(&mut self, parser: Box<dyn ReferenceParser>) {
        self.by_scheme.insert(parser.scheme(), parser);
    }

    /// The parser registered for `scheme`, if any.
    pub fn get(&self, scheme: &str) -> Option<&dyn ReferenceParser> {
        self.by_scheme.get(scheme).map(|b| b.as_ref())
    }

    /// Every registered scheme, in arbitrary order.
    pub fn schemes(&self) -> impl Iterator<Item = &str> + '_ {
        self.by_scheme.keys().copied()
    }
}

/// The built-in `nostr:` reference parser: resolves a bech32 entity
/// (`nevent1…`/`note1…`/`naddr1…`) via [`resolve_ref`](crate::resolve_ref),
/// reproducing the behaviour the markdown scanner hardcoded before schemes
/// existed. A zero-sized, stateless parser.
pub struct NostrRefParser;

impl ReferenceParser for NostrRefParser {
    fn scheme(&self) -> &'static str {
        "nostr"
    }

    fn token_len(&self, rest: &str) -> usize {
        // A bech32 token is a run of lowercase letters/digits (hrp + data) —
        // exactly the delimiter the scanner used for `nostr:` before.
        rest.find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
            .unwrap_or(rest.len())
    }

    fn resolve(&self, token: &str, ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
        let note = crate::resolve_ref(ctx.ndb, ctx.txn, token)?;
        Some(ResolvedRef::note(NoteId::new(*note.id())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubParser;
    impl ReferenceParser for StubParser {
        fn scheme(&self) -> &'static str {
            "stub"
        }
        fn token_len(&self, rest: &str) -> usize {
            // Token runs until whitespace.
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        }
        fn resolve(&self, _token: &str, _ctx: &ReferenceResolveCtx) -> Option<ResolvedRef> {
            None
        }
    }

    #[test]
    fn default_registry_has_builtin_nostr() {
        let reg = ReferenceParserRegistry::default();
        assert!(reg.get("nostr").is_some());
        assert_eq!(reg.get("nostr").unwrap().scheme(), "nostr");
        assert!(reg.get("headway").is_none());
    }

    #[test]
    fn register_adds_scheme_alongside_builtin() {
        let mut reg = ReferenceParserRegistry::default();
        reg.register(Box::new(StubParser));
        assert!(reg.get("nostr").is_some());
        assert!(reg.get("stub").is_some());
        let mut schemes: Vec<_> = reg.schemes().collect();
        schemes.sort_unstable();
        assert_eq!(schemes, vec!["nostr", "stub"]);
    }

    #[test]
    fn nostr_token_len_stops_at_non_bech32() {
        let p = NostrRefParser;
        // Stops at the space; the trailing prose is not part of the token.
        assert_eq!(p.token_len("nevent1abc def"), "nevent1abc".len());
        // A bare scheme with no token yields 0.
        assert_eq!(p.token_len(" trailing"), 0);
        // Uppercase is outside the bech32 class, ending the token.
        assert_eq!(p.token_len("note1xyzABC"), "note1xyz".len());
    }

    #[test]
    fn stub_token_len_uses_its_own_grammar() {
        let p = StubParser;
        assert_eq!(
            p.token_len("board#maple-river more"),
            "board#maple-river".len()
        );
    }
}
