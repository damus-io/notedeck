//! The chrome's keymap: key sequences bound to tools, per key scope.
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
//! [`DEFAULT_KEYMAP`] (`default_keymap.toml`). After the app's scopes comes
//! [`GLOBAL_SCOPE`], for the chrome's own bindings (`?` for the which-key
//! strip), which [`Keymap::hints`] draws from the same bindings.
//!
//! A binding can call a **data tool** too (an app's [`notedeck::AppTool`],
//! e.g. `headway_archive_card`), on whatever the app's view is on: a string
//! in its `args` naming [`CURRENT`] (`args = { card = "{current}" }`) is
//! replaced, as the key fires, with the active app's
//! [current ref](notedeck::App::current_ref). A binding naming it is ready
//! only while the app has one, so without one its keys fall through.
//!
//! Text-field keys stay with the apps: the engine does nothing while a widget
//! has keyboard focus, a popup is open or a drag is under way (see
//! [`notedeck_ui::chord::focus_taken`]).

mod hints;
pub mod keys;
mod parse;

use std::collections::BTreeMap;

use notedeck::ToolInvocation;
use notedeck_ui::chord::{self, ChordState};

pub use hints::{Hint, MAX_HINT_KEYS};
pub use keys::{KeyParseError, KeySeq, KeyStroke, Mods, MAX_SEQ};
pub use parse::{parse, BindProblem, KeymapError};

/// The scope searched after every app's own: the chrome's bindings, live in
/// any app.
pub const GLOBAL_SCOPE: &str = "global";

/// The placeholder a binding's `args` name the active app's
/// [current ref](notedeck::App::current_ref) with, replaced as the key fires.
pub const CURRENT: &str = "{current}";

/// The scopes a key is resolved in, most specific first: the active app's
/// [key scopes](notedeck::App::key_scopes), then [`GLOBAL_SCOPE`]; and the
/// app's [current ref](notedeck::App::current_ref), for the bindings whose
/// args name [`CURRENT`]. `Copy`, and walking it doesn't allocate.
#[derive(Clone, Copy, Debug)]
pub struct Scopes<'a> {
    /// The app's scopes, most specific first.
    app: &'a [&'a str],
    /// What the app's view is on, e.g. `headway:<board>/<word-id>`.
    current: Option<&'a str>,
}

impl<'a> Scopes<'a> {
    /// The app's `scopes`, then [`GLOBAL_SCOPE`], with no current ref.
    pub fn new(app: &'a [&'a str]) -> Self {
        Self { app, current: None }
    }

    /// These scopes with the app's `current` ref.
    pub fn with_current(self, current: Option<&'a str>) -> Self {
        Self { current, ..self }
    }

    /// The app's current ref, if it has one.
    pub fn current(self) -> Option<&'a str> {
        self.current
    }

    /// How many scopes there are, the global one included.
    fn len(self) -> usize {
        self.app.len() + 1
    }

    /// The scope at `index`, most specific first.
    fn get(self, index: usize) -> Option<&'a str> {
        match self.app.get(index) {
            Some(scope) => Some(scope),
            None => (index == self.app.len()).then_some(GLOBAL_SCOPE),
        }
    }

    /// The scopes in order.
    fn iter(self) -> impl Iterator<Item = &'a str> + Clone {
        self.app
            .iter()
            .copied()
            .chain(std::iter::once(GLOBAL_SCOPE))
    }
}

/// A bare Esc, which cancels a pending chord.
const ESCAPE: KeyStroke = KeyStroke {
    key: egui::Key::Escape,
    mods: Mods::NONE,
};

/// The built-in keymap, compiled in: the chrome's global bindings and
/// Headway's and Dave's; a user's overrides will be merged over it.
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
    /// The bindings as the which-key strip lists them: by `group` in the
    /// order the file first names each, then in file order. Built once at
    /// load, so the strip never sorts.
    groups: Vec<GroupRun>,
}

/// One which-key group of a [`Scope`].
#[derive(Debug)]
struct GroupRun {
    /// The `group` its bindings share; `None` for the ungrouped.
    name: Option<String>,
    /// Indices into the scope's `bindings`, in file order.
    members: Vec<usize>,
}

impl Scope {
    /// Its run of the group `name`, if it has bindings in it.
    fn group(&self, name: Option<&str>) -> Option<&GroupRun> {
        self.groups.iter().find(|run| run.name.as_deref() == name)
    }
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
    /// Its position in the file, for the which-key strip's order.
    order: usize,
    /// The which-key keycap of `keys` from each stroke on (`gg`, then `g`),
    /// so the strip can show what's left of a chord without formatting.
    keycaps: Vec<String>,
    /// Whether its tool's args name [`CURRENT`], read once at load.
    uses_current: bool,
}

impl Binding {
    /// Whether it would run now: its tool is `available`, and the app has a
    /// current ref if its args name one. Whether a macro can run is its
    /// runner's call, so a macro binding always is.
    fn ready(&self, scopes: Scopes<'_>, available: &impl Fn(&str) -> bool) -> bool {
        match &self.action {
            BindAction::Tool(invocation) => {
                available(&invocation.name) && (!self.uses_current || scopes.current.is_some())
            }
            BindAction::Macro(_) => true,
        }
    }

    /// The which-key keycap of the keys left after the first `typed`: `gg`
    /// for 0, `g` for 1 of `g g`. Empty when `typed` covers them all.
    pub fn keycap(&self, typed: usize) -> &str {
        self.keycaps.get(typed).map_or("", String::as_str)
    }
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
        let Some(scope) = self.scope(scope) else {
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

    /// Every binding of the scope `name`, sorted by keys; empty for a scope
    /// the keymap doesn't name. For tests and an editor that walk a scope.
    pub fn bindings(&self, scope: &str) -> &[Binding] {
        self.scope(scope).map_or(&[], |scope| &scope.bindings)
    }

    /// The bindings of the scope called `name`.
    fn scope(&self, name: &str) -> Option<&Scope> {
        self.scopes.iter().find(|scope| scope.name == name)
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
    /// Call this tool, a UI tool or a data tool, its args' [`CURRENT`]
    /// already replaced with the app's current ref.
    Tool(ToolInvocation),
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

    /// Resolve this frame's key press against the app's `scopes` (most
    /// specific first) and then [`GLOBAL_SCOPE`], asking `available` whether a
    /// bound tool does anything right now. A binding whose args name
    /// [`CURRENT`] is passed over while `scopes` has no current ref.
    ///
    /// Does nothing while focus is taken (a text field, popup or drag). A press
    /// that completes a binding is swallowed and returned; one that starts or
    /// extends a chord is swallowed and kept; anything else clears the chord
    /// and is left for the app. A chord that goes nowhere starts over from the
    /// key that broke it, so `g j` with only `g g` and `j` bound still runs
    /// `j`, except a bare Esc, which only cancels the chord, as in vim, and is
    /// swallowed. A frame with no key press doesn't allocate.
    #[profiling::function]
    pub fn resolve(
        &mut self,
        ctx: &egui::Context,
        scopes: Scopes<'_>,
        available: impl Fn(&str) -> bool,
    ) -> Option<KeyDispatch<'_>> {
        let pending = self.chord.tick(ctx);
        if chord::focus_taken(ctx) {
            self.chord.clear();
            return None;
        }
        let stroke = KeyStroke::from_press(ctx.input(chord::first_key_press)?);

        let fresh = KeySeq::single(stroke);
        let (seq, found) = match pending.and_then(|prefix| prefix.pushed(stroke)) {
            Some(seq) => match find(&self.keymap, scopes, seq.as_slice(), &available) {
                Found::None if stroke == ESCAPE => {
                    self.chord.clear();
                    chord::swallow_key_events(ctx);
                    return None;
                }
                Found::None => (
                    fresh,
                    find(&self.keymap, scopes, fresh.as_slice(), &available),
                ),
                found => (seq, found),
            },
            None => (
                fresh,
                find(&self.keymap, scopes, fresh.as_slice(), &available),
            ),
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
                    BindAction::Tool(invocation) => {
                        KeyDispatch::Tool(with_current(invocation, binding, scopes.current))
                    }
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
    scopes: Scopes<'_>,
    seq: &[KeyStroke],
    available: &impl Fn(&str) -> bool,
) -> Found<'a> {
    for scope in scopes.iter() {
        match keymap.lookup(scope, seq) {
            Match::Exact(bindings) => {
                let ready = bindings.iter().find(|b| b.ready(scopes, available));
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

/// `invocation`, `binding`'s tool call, as the key fires it: its args with
/// every [`CURRENT`] replaced by `current`. Only a fired binding is ever
/// substituted, so this is the one place a key press allocates its call.
fn with_current(
    invocation: &ToolInvocation,
    binding: &Binding,
    current: Option<&str>,
) -> ToolInvocation {
    match current {
        Some(current) if binding.uses_current => ToolInvocation::new(
            invocation.name.clone(),
            replace_current(&invocation.args, current),
        ),
        _ => invocation.clone(),
    }
}

/// `args` with [`CURRENT`] replaced by `current` in every string, however
/// deep.
fn replace_current(args: &serde_json::Value, current: &str) -> serde_json::Value {
    use serde_json::Value;
    match args {
        Value::String(text) => Value::String(text.replace(CURRENT, current)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| replace_current(item, current))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), replace_current(value, current)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Whether any string in `args`, however deep, names [`CURRENT`].
fn names_current(args: &serde_json::Value) -> bool {
    use serde_json::Value;
    match args {
        Value::String(text) => text.contains(CURRENT),
        Value::Array(items) => items.iter().any(names_current),
        Value::Object(fields) => fields.values().any(names_current),
        _ => false,
    }
}
