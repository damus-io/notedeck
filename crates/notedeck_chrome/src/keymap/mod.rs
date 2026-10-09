//! The chrome's keymap: key sequences bound to UI tools, per key scope.
//!
//! Apps advertise UI tools ([`notedeck::App::ui_tools`]) and report which key
//! scopes are active each frame ([`notedeck::App::key_scopes`], most specific
//! first). The keymap binds key sequences in a scope to a tool call or a
//! macro. Each frame, before the active app renders, the chrome's
//! [`KeyEngine`] reads the first key press, resolves it against the active
//! scopes, swallows it on a match and hands back the tool to call, so the app
//! never sees a key the keymap took.
//!
//! The keymap is TOML ([`parse`] has the format), parsed once into a
//! [`Keymap`] whose lookups don't allocate. The built-in bindings live in
//! [`DEFAULT_KEYMAP`] (`default_keymap.toml`).
//!
//! Text-field keys stay with the apps: the engine does nothing while a widget
//! has keyboard focus, a popup is open or a drag is under way (see
//! [`notedeck_ui::chord::focus_taken`]).

pub mod keys;
mod parse;

use std::collections::BTreeMap;

use notedeck::ToolInvocation;
use notedeck_ui::chord::{self, ChordState};

pub use keys::{KeyParseError, KeySeq, KeyStroke, Mods, MAX_SEQ};
pub use parse::{parse, BindProblem, KeymapError};

/// The built-in keymap, compiled in. Empty until the apps' bindings move into
/// it; a user's overrides will be merged over it.
pub const DEFAULT_KEYMAP: &str = include_str!("default_keymap.toml");

/// A parsed keymap: bindings grouped by scope, plus the named macros.
#[derive(Debug, Default)]
pub struct Keymap {
    /// One entry per scope, in the order the file first names them.
    scopes: Vec<Scope>,
    /// `[macro.<name>]` tables by name.
    macros: BTreeMap<String, Macro>,
    /// Load-time lints that didn't fail the parse (a binding shadowed by its
    /// own prefix), for an editor to show.
    warnings: Vec<String>,
}

/// The bindings of one key scope.
#[derive(Debug)]
struct Scope {
    /// The scope's name, e.g. `headway.grid`.
    name: String,
    /// Sorted by key sequence; bindings of the same keys keep file order.
    bindings: Vec<Binding>,
}

/// One key sequence bound to something in a scope.
#[derive(Debug)]
pub struct Binding {
    /// The keys that trigger it.
    pub keys: KeySeq,
    /// What they trigger.
    pub action: BindAction,
    /// Its which-key label.
    pub label: Option<String>,
    /// Its which-key group.
    pub group: Option<String>,
}

/// What a binding triggers.
#[derive(Debug)]
pub enum BindAction {
    /// Call one UI tool with these args.
    Tool(ToolInvocation),
    /// Run the named macro (see [`Keymap::macro_named`]).
    Macro(String),
}

/// A named sequence of tool calls.
#[derive(Debug)]
pub struct Macro {
    /// Its which-key label.
    pub label: Option<String>,
    /// The calls, in order. Never empty.
    pub steps: Vec<MacroStep>,
}

/// One call in a [`Macro`].
#[derive(Debug)]
pub struct MacroStep {
    /// The tool and its args.
    pub invocation: ToolInvocation,
}

/// What a key sequence is in one scope.
#[derive(Debug)]
pub enum Match<'a> {
    /// Bound: the bindings of exactly these keys, in file order.
    Exact(&'a [Binding]),
    /// Not bound itself, but the start of a longer binding.
    Prefix,
    /// Neither.
    None,
}

impl Keymap {
    /// Whether no key is bound anywhere.
    pub fn is_empty(&self) -> bool {
        self.scopes.iter().all(|scope| scope.bindings.is_empty())
    }

    /// What `keys` is in `scope`. Allocation-free.
    pub fn lookup(&self, scope: &str, keys: &[KeyStroke]) -> Match<'_> {
        let Some(scope) = self.scopes.iter().find(|s| s.name == scope) else {
            return Match::None;
        };
        let bindings = &scope.bindings;
        let start = bindings.partition_point(|b| b.keys.as_slice() < keys);
        let rest = &bindings[start..];
        let exact = rest.partition_point(|b| b.keys.as_slice() == keys);
        if exact > 0 {
            return Match::Exact(&rest[..exact]);
        }
        // Sorted, so the first binding past `keys` is the one that would
        // extend it.
        match rest.first() {
            Some(next) if next.keys.as_slice().starts_with(keys) => Match::Prefix,
            _ => Match::None,
        }
    }

    /// The macro called `name`.
    pub fn macro_named(&self, name: &str) -> Option<&Macro> {
        self.macros.get(name)
    }

    /// Lints from loading it: bindings dropped because a shorter binding in
    /// the same scope is their prefix.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// What a resolved key press asks the chrome to do.
#[derive(Debug, PartialEq)]
pub enum KeyDispatch<'a> {
    /// Call this UI tool.
    Tool(&'a ToolInvocation),
    /// Run the macro with this name.
    Macro(&'a str),
}

/// The per-frame key resolver: a [`Keymap`] and the chord prefix typed so far.
/// Owned by the chrome.
#[derive(Default)]
pub struct KeyEngine {
    /// The bindings.
    keymap: Keymap,
    /// The keys of a chord in progress (`g` of `g g`), lapsing after
    /// [`chord::CHORD_TIMEOUT`].
    chord: ChordState<KeySeq>,
}

/// What the active scopes make of a key sequence.
enum Found<'a> {
    /// A binding to run.
    Fire(&'a Binding),
    /// The start of a longer binding.
    Prefix,
    /// Nothing.
    None,
}

impl KeyEngine {
    /// An engine resolving against `keymap`.
    pub fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            chord: ChordState::default(),
        }
    }

    /// An engine with the built-in [`DEFAULT_KEYMAP`]. A default that doesn't
    /// parse is logged and leaves the keymap empty rather than failing.
    pub fn with_default_keymap() -> Self {
        let keymap = parse(DEFAULT_KEYMAP).unwrap_or_else(|err| {
            tracing::error!("default keymap: {err}; no key bindings");
            Keymap::default()
        });
        Self::new(keymap)
    }

    /// The bindings in use.
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Replace the bindings, dropping any chord in progress.
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
        self.chord.clear();
    }

    /// The keys of a chord in progress, if any.
    pub fn pending(&self) -> Option<KeySeq> {
        self.chord.pending()
    }

    /// Resolve this frame's key press against `scopes` (most specific first),
    /// asking `available` whether a bound tool does anything right now.
    ///
    /// Does nothing while focus is taken (a text field, popup or drag). A press
    /// that completes a binding is swallowed and returned; one that starts or
    /// extends a chord is swallowed and kept; anything else clears the chord
    /// and is left for the app. A chord that goes nowhere starts over from the
    /// key that broke it, so `g j` with only `g g` and `j` bound still runs
    /// `j`. A frame with no key press doesn't allocate.
    #[profiling::function]
    pub fn resolve(
        &mut self,
        ctx: &egui::Context,
        scopes: &[&str],
        available: impl Fn(&str) -> bool,
    ) -> Option<KeyDispatch<'_>> {
        let pending = self.chord.tick(ctx);
        if scopes.is_empty() || chord::focus_taken(ctx) {
            self.chord.clear();
            return None;
        }
        let stroke = KeyStroke::from_press(ctx.input(chord::first_key_press)?);

        let fresh = KeySeq::single(stroke);
        let (seq, found) = match pending.and_then(|prefix| prefix.pushed(stroke)) {
            Some(seq) => match find(&self.keymap, scopes, &seq, &available) {
                Found::None => (fresh, find(&self.keymap, scopes, &fresh, &available)),
                found => (seq, found),
            },
            None => (fresh, find(&self.keymap, scopes, &fresh, &available)),
        };

        match found {
            Found::None => {
                self.chord.clear();
                None
            }
            Found::Prefix => {
                self.chord.begin(seq, ctx.input(|i| i.time));
                chord::swallow_key_events(ctx);
                None
            }
            Found::Fire(binding) => {
                self.chord.clear();
                chord::swallow_key_events(ctx);
                Some(match &binding.action {
                    BindAction::Tool(invocation) => KeyDispatch::Tool(invocation),
                    BindAction::Macro(name) => KeyDispatch::Macro(name),
                })
            }
        }
    }
}

/// Look `seq` up in each of `scopes` in turn. The first binding whose tool is
/// `available` fires; an unavailable one falls through to the next binding of
/// the same keys, then the next scope. A prefix in an earlier scope wins over
/// anything in a later one.
fn find<'a>(
    keymap: &'a Keymap,
    scopes: &[&str],
    seq: &KeySeq,
    available: &impl Fn(&str) -> bool,
) -> Found<'a> {
    for scope in scopes {
        match keymap.lookup(scope, seq.as_slice()) {
            Match::Exact(bindings) => {
                let ready = bindings.iter().find(|b| match &b.action {
                    BindAction::Tool(invocation) => available(&invocation.name),
                    // Whether a macro can run is its runner's call.
                    BindAction::Macro(_) => true,
                });
                if let Some(binding) = ready {
                    return Found::Fire(binding);
                }
            }
            Match::Prefix => return Found::Prefix,
            Match::None => {}
        }
    }
    Found::None
}
