//! TOML → [`Keymap`].
//!
//! ```toml
//! [[bind]]
//! scope = "headway.grid"
//! keys  = "g g"
//! tool  = "headway_cursor_first"
//! label = "first card"      # which-key label
//! group = "move"            # which-key group
//! args  = { count = 1 }     # optional; passed to the tool as JSON
//!
//! [[bind]]
//! scope = "headway.card"
//! keys  = "a"
//! tool  = "headway_archive_card"     # a data tool, on the current card:
//! args  = { card = "{current}" }     # "{current}" is the app's current ref
//!
//! [[bind]]
//! scope = "dave.normal"
//! keys  = "E"
//! macro = "session-explainer"
//!
//! [macro.session-explainer]
//! label = "card explainer"
//! steps = [ { tool = "dave_open_issue" }, { tool = "headway_open_explainer" } ]
//! ```
//!
//! Anything malformed — bad TOML, an unknown field, a binding with both or
//! neither of `tool`/`macro`, a key sequence that doesn't parse, a binding to
//! an undefined macro — fails the whole parse with a message naming the
//! binding, so an editor can reject the save. A binding that is a prefix of
//! another in the same scope is only a lint: see [`Keymap::warnings`].
//!
//! A user's keymap is merged over the defaults with [`parse_over`], in the
//! same format plus one field:
//!
//! ```toml
//! [[bind]]                  # replaces every default binding of `j` in
//! scope = "headway.grid"    # headway.grid
//! keys  = "j"
//! tool  = "headway_cursor_down"
//!
//! [[bind]]                  # removes the default binding of `x`
//! scope = "headway.grid"
//! keys  = "x"
//! unbind = true
//! ```
//!
//! A user `[macro.<name>]` adds a macro or replaces the default of that name.

use std::collections::BTreeMap;
use std::fmt;

use notedeck::ToolInvocation;
use serde::Deserialize;

use super::keys::{KeyParseError, KeySeq};
use super::{BindAction, Binding, GroupRun, Keymap, Macro, MacroStep, Scope};

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeymap {
    /// `[[bind]]` entries, in file order.
    #[serde(default)]
    bind: Vec<RawBind>,
    /// `[macro.<name>]` tables.
    #[serde(default, rename = "macro")]
    macros: BTreeMap<String, RawMacro>,
}

/// One `[[bind]]`.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBind {
    scope: String,
    keys: String,
    tool: Option<String>,
    #[serde(rename = "macro")]
    macro_name: Option<String>,
    label: Option<String>,
    group: Option<String>,
    args: Option<toml::Table>,
    /// In a user keymap: remove the default binding of these keys rather
    /// than bind them (see [`parse_over`]).
    #[serde(default)]
    unbind: bool,
    /// Its 1-based position among the `[[bind]]`s of the file it came from,
    /// for error messages; set by [`read`], so it survives a merge.
    #[serde(skip)]
    index: usize,
}

impl RawBind {
    /// The error for this binding having `problem`.
    fn fail(&self, problem: BindProblem) -> KeymapError {
        KeymapError::Bind {
            index: self.index,
            scope: self.scope.clone(),
            keys: self.keys.clone(),
            problem,
        }
    }
}

/// One `[macro.<name>]`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMacro {
    label: Option<String>,
    steps: Vec<RawStep>,
}

/// One entry of a macro's `steps`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    tool: String,
    args: Option<toml::Table>,
}

/// Why a keymap didn't parse.
#[derive(Debug)]
pub enum KeymapError {
    /// Not valid TOML, or not the keymap's shape.
    Toml(toml::de::Error),
    /// A `[[bind]]` that is wrong on its own.
    Bind {
        /// Its 1-based position among the `[[bind]]`s.
        index: usize,
        /// Its scope, to find it by.
        scope: String,
        /// Its keys as written.
        keys: String,
        /// What is wrong with it.
        problem: BindProblem,
    },
    /// A `[macro.<name>]` with no steps.
    EmptyMacro(String),
    /// In a user keymap, a macro step names a tool no app has.
    UnknownStepTool {
        /// The macro's name.
        name: String,
        /// The step's 1-based position.
        step: usize,
        /// The tool it names.
        tool: String,
    },
}

/// What is wrong with one `[[bind]]`.
#[derive(Debug)]
pub enum BindProblem {
    /// `keys` doesn't parse.
    Keys(KeyParseError),
    /// Neither `tool` nor `macro`.
    NoAction,
    /// Both `tool` and `macro`.
    BothActions,
    /// `macro` names no `[macro.<name>]`.
    UnknownMacro(String),
    /// `args` on a macro binding, which takes its args from its steps.
    MacroArgs,
    /// `unbind` outside a user keymap, where there is nothing to unbind.
    Unbind,
    /// `unbind` with a `tool` or `macro`.
    UnbindWithAction,
    /// In a user keymap, `tool` names a tool no app has.
    UnknownTool(String),
}

impl fmt::Display for KeymapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(err) => write!(f, "{err}"),
            Self::Bind {
                index,
                scope,
                keys,
                problem,
            } => {
                write!(f, "bind #{index} (scope '{scope}', keys '{keys}'): ")?;
                match problem {
                    BindProblem::Keys(err) => write!(f, "{err}"),
                    BindProblem::NoAction => write!(f, "needs a 'tool' or a 'macro'"),
                    BindProblem::BothActions => {
                        write!(f, "has both 'tool' and 'macro'; pick one")
                    }
                    BindProblem::UnknownMacro(name) => {
                        write!(f, "macro '{name}' is not defined (add [macro.{name}])")
                    }
                    BindProblem::MacroArgs => write!(
                        f,
                        "'args' goes on the macro's steps, not on a macro binding"
                    ),
                    BindProblem::Unbind => {
                        write!(f, "'unbind' only applies to a user keymap")
                    }
                    BindProblem::UnbindWithAction => {
                        write!(f, "'unbind' takes no 'tool' or 'macro'")
                    }
                    BindProblem::UnknownTool(tool) => write!(f, "no app has a tool '{tool}'"),
                }
            }
            Self::EmptyMacro(name) => write!(f, "macro '{name}' has no steps"),
            Self::UnknownStepTool { name, step, tool } => {
                write!(f, "macro '{name}' step {step}: no app has a tool '{tool}'")
            }
        }
    }
}

impl std::error::Error for KeymapError {}

/// Parse a keymap from TOML. See the module docs for the format.
pub fn parse(src: &str) -> Result<Keymap, KeymapError> {
    build(read(src)?, Vec::new())
}

/// Parse the `user` keymap merged over `defaults`; see the module docs for
/// what a user keymap can say. `known` says whether some app has a tool by a
/// name: a user binding or macro step naming one it doesn't fails the whole
/// merge, so a typo never quietly drops a key. Anything wrong fails it all,
/// never half-applies; an `unbind` that matches no default binding is only a
/// warning.
pub fn parse_over(
    defaults: &str,
    user: &str,
    known: impl Fn(&str) -> bool,
) -> Result<Keymap, KeymapError> {
    let mut merged = read(defaults)?;
    let user = read(user)?;
    check_user(&user, &known)?;
    let mut warnings = Vec::new();
    merged.bind = merge_binds(merged.bind, user.bind, &mut warnings)?;
    merged.macros.extend(user.macros);
    build(merged, warnings)
}

/// `src` as written, each `[[bind]]` numbered.
fn read(src: &str) -> Result<RawKeymap, KeymapError> {
    let mut raw: RawKeymap = toml::from_str(src).map_err(KeymapError::Toml)?;
    for (i, bind) in raw.bind.iter_mut().enumerate() {
        bind.index = i + 1;
    }
    Ok(raw)
}

/// What only a user keymap is checked for: an `unbind` that also binds, and
/// a tool `known` says no app has.
fn check_user(user: &RawKeymap, known: &impl Fn(&str) -> bool) -> Result<(), KeymapError> {
    for bind in &user.bind {
        if bind.unbind && (bind.tool.is_some() || bind.macro_name.is_some()) {
            return Err(bind.fail(BindProblem::UnbindWithAction));
        }
        if let Some(tool) = bind.tool.as_deref().filter(|tool| !known(tool)) {
            return Err(bind.fail(BindProblem::UnknownTool(tool.to_owned())));
        }
    }
    for (name, m) in &user.macros {
        let unknown = m.steps.iter().enumerate().find(|(_, s)| !known(&s.tool));
        if let Some((i, step)) = unknown {
            return Err(KeymapError::UnknownStepTool {
                name: name.clone(),
                step: i + 1,
                tool: step.tool.clone(),
            });
        }
    }
    Ok(())
}

/// A user `[[bind]]` waiting to be merged.
struct Override {
    /// Its keys, parsed, to match the defaults' by.
    keys: KeySeq,
    bind: RawBind,
    /// Whether it has replaced a default binding yet.
    placed: bool,
}

/// The `defaults`' bindings with the `user`'s merged over them: every default
/// binding of a scope and keys the user binds is dropped, and the user's
/// bindings of them take the place of the first, so the which-key strip
/// keeps its order. User bindings of keys no default binds go at the end; an
/// `unbind` only drops, and one that drops nothing is a warning.
fn merge_binds(
    defaults: Vec<RawBind>,
    user: Vec<RawBind>,
    warnings: &mut Vec<String>,
) -> Result<Vec<RawBind>, KeymapError> {
    let mut overrides = user
        .into_iter()
        .map(|bind| match KeySeq::parse(&bind.keys) {
            Ok(keys) => Ok(Override {
                keys,
                bind,
                placed: false,
            }),
            Err(err) => Err(bind.fail(BindProblem::Keys(err))),
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut merged = Vec::with_capacity(defaults.len() + overrides.len());
    for bind in defaults {
        // A default that doesn't parse is kept for `build` to report.
        let keys = KeySeq::parse(&bind.keys).ok();
        let mut replaced = false;
        for o in &mut overrides {
            if o.bind.scope != bind.scope || Some(o.keys) != keys {
                continue;
            }
            replaced = true;
            if !o.placed && !o.bind.unbind {
                merged.push(o.bind.clone());
            }
            o.placed = true;
        }
        if !replaced {
            merged.push(bind);
        }
    }
    for o in overrides.into_iter().filter(|o| !o.placed) {
        if o.bind.unbind {
            warnings.push(format!(
                "in scope '{}', unbind '{}' matches no default binding",
                o.bind.scope, o.bind.keys
            ));
        } else {
            merged.push(o.bind);
        }
    }
    Ok(merged)
}

/// The [`Keymap`] `raw` describes, with `warnings` from merging it.
fn build(raw: RawKeymap, mut warnings: Vec<String>) -> Result<Keymap, KeymapError> {
    let mut macros = BTreeMap::new();
    for (name, raw_macro) in raw.macros {
        if raw_macro.steps.is_empty() {
            return Err(KeymapError::EmptyMacro(name));
        }
        let steps = raw_macro
            .steps
            .into_iter()
            .map(|step| MacroStep {
                invocation: ToolInvocation::new(step.tool, args_json(step.args)),
            })
            .collect();
        let label = raw_macro.label;
        macros.insert(name, Macro { label, steps });
    }

    let mut scopes: Vec<Scope> = Vec::new();
    for (i, bind) in raw.bind.into_iter().enumerate() {
        let keys = KeySeq::parse(&bind.keys).map_err(|e| bind.fail(BindProblem::Keys(e)))?;
        if bind.unbind {
            return Err(bind.fail(BindProblem::Unbind));
        }
        let action = match (&bind.tool, &bind.macro_name) {
            (None, None) => return Err(bind.fail(BindProblem::NoAction)),
            (Some(_), Some(_)) => return Err(bind.fail(BindProblem::BothActions)),
            (None, Some(name)) if !macros.contains_key(name) => {
                return Err(bind.fail(BindProblem::UnknownMacro(name.clone())))
            }
            (None, Some(_)) if bind.args.is_some() => return Err(bind.fail(BindProblem::MacroArgs)),
            (None, Some(name)) => BindAction::Macro(name.clone()),
            (Some(tool), None) => {
                BindAction::Tool(ToolInvocation::new(tool.clone(), args_json(bind.args)))
            }
        };

        let uses_current = match &action {
            BindAction::Tool(invocation) => super::names_current(&invocation.args),
            BindAction::Macro(_) => false,
        };
        let binding = Binding {
            keys,
            action,
            uses_current,
            label: bind.label,
            group: bind.group,
            order: i,
            keycaps: (0..keys.as_slice().len())
                .map(|from| keys.keycap(from))
                .collect(),
        };
        match scopes.iter_mut().find(|s| s.name == bind.scope) {
            Some(scope) => scope.bindings.push(binding),
            None => scopes.push(Scope {
                name: bind.scope,
                bindings: vec![binding],
                groups: Vec::new(),
            }),
        }
    }

    for scope in &mut scopes {
        // Stable, so bindings of the same keys keep their file order: the
        // first one whose tool is available wins.
        scope
            .bindings
            .sort_by(|a, b| a.keys.as_slice().cmp(b.keys.as_slice()));
        drop_shadowed(scope, &mut warnings);
        scope.groups = group_runs(&scope.bindings);
    }
    for warning in &warnings {
        tracing::warn!("keymap: {warning}");
    }

    Ok(Keymap {
        scopes,
        macros,
        warnings,
    })
}

/// Drop every binding in `scope` that a shorter binding is a prefix of
/// (`s e` under `s`): the shorter one fires on its last key, so the longer one
/// could never be reached. Keeps the shorter and records why in `warnings`.
///
/// `scope.bindings` must be sorted, so each binding's prefixes come before it.
fn drop_shadowed(scope: &mut Scope, warnings: &mut Vec<String>) {
    let mut kept: Vec<Binding> = Vec::with_capacity(scope.bindings.len());
    for binding in scope.bindings.drain(..) {
        let keys = binding.keys.as_slice();
        let shadow = kept.iter().find(|k| {
            let prefix = k.keys.as_slice();
            prefix.len() < keys.len() && keys.starts_with(prefix)
        });
        match shadow {
            Some(shorter) => warnings.push(format!(
                "in scope '{}', '{}' can never fire because '{}' is bound; dropped it",
                scope.name, binding.keys, shorter.keys
            )),
            None => kept.push(binding),
        }
    }
    scope.bindings = kept;
}

/// The which-key groups of a scope's `bindings`: one run per `group`, in the
/// order the file first names each, members in file order.
fn group_runs(bindings: &[Binding]) -> Vec<GroupRun> {
    let mut by_file: Vec<usize> = (0..bindings.len()).collect();
    by_file.sort_by_key(|&i| bindings[i].order);

    let mut runs: Vec<GroupRun> = Vec::new();
    for i in by_file {
        let name = bindings[i].group.as_deref();
        match runs.iter_mut().find(|run| run.name.as_deref() == name) {
            Some(run) => run.members.push(i),
            None => runs.push(GroupRun {
                name: name.map(str::to_owned),
                members: vec![i],
            }),
        }
    }
    runs
}

/// A binding's or step's `args` table as the tool's JSON args; `{}` when
/// absent, so a tool with all-optional args deserializes either way.
fn args_json(args: Option<toml::Table>) -> serde_json::Value {
    let Some(args) = args else {
        return serde_json::Value::Object(serde_json::Map::new());
    };
    // A TOML table always has a JSON form; datetimes become strings.
    serde_json::to_value(args).unwrap_or_else(|err| {
        tracing::warn!("keymap: args not representable as JSON: {err}");
        serde_json::Value::Object(serde_json::Map::new())
    })
}

#[cfg(test)]
mod tests {
    use super::super::keys::KeyStroke;
    use super::super::Match;
    use super::*;
    use serde_json::json;

    fn keys(text: &str) -> KeySeq {
        KeySeq::parse(text).expect("keys")
    }

    /// The tool a lookup's first exact binding runs.
    fn tool_at(keymap: &Keymap, scope: &str, text: &str) -> Option<String> {
        match keymap.lookup(scope, keys(text).as_slice()) {
            Match::Exact(bindings) => match &bindings[0].action {
                BindAction::Tool(invocation) => Some(invocation.name.clone()),
                BindAction::Macro(name) => Some(format!("macro:{name}")),
            },
            _ => None,
        }
    }

    #[test]
    fn an_empty_keymap_parses() {
        let keymap = parse("").expect("parse");
        assert!(keymap.is_empty());
        assert!(keymap.warnings().is_empty());
    }

    #[test]
    fn the_shipped_default_keymap_parses() {
        parse(super::super::DEFAULT_KEYMAP).expect("default_keymap.toml");
    }

    #[test]
    fn bindings_resolve_by_scope_and_keys() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "grid"
            keys = "j"
            tool = "down"
            label = "down"
            group = "move"

            [[bind]]
            scope = "grid"
            keys = "g g"
            tool = "first"

            [[bind]]
            scope = "card"
            keys = "j"
            tool = "next_field"
            "#,
        )
        .expect("parse");

        assert_eq!(tool_at(&keymap, "grid", "j").as_deref(), Some("down"));
        assert_eq!(tool_at(&keymap, "card", "j").as_deref(), Some("next_field"));
        assert_eq!(tool_at(&keymap, "grid", "g g").as_deref(), Some("first"));
        assert!(matches!(
            keymap.lookup("grid", keys("g").as_slice()),
            Match::Prefix
        ));
        assert!(matches!(
            keymap.lookup("grid", keys("k").as_slice()),
            Match::None
        ));
        assert!(matches!(
            keymap.lookup("nowhere", keys("j").as_slice()),
            Match::None
        ));

        let Match::Exact([binding]) = keymap.lookup("grid", keys("j").as_slice()) else {
            panic!("one binding for j");
        };
        assert_eq!(binding.label.as_deref(), Some("down"));
        assert_eq!(binding.group.as_deref(), Some("move"));
    }

    #[test]
    fn args_become_json_and_default_to_an_empty_object() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "s"
            keys = "x"
            tool = "t"
            args = { count = 3, to = "done", deep = { flag = true }, list = [1, 2] }

            [[bind]]
            scope = "s"
            keys = "y"
            tool = "t"
            "#,
        )
        .expect("parse");

        let args = |text: &str| match keymap.lookup("s", keys(text).as_slice()) {
            Match::Exact(
                [Binding {
                    action: BindAction::Tool(invocation),
                    ..
                }],
            ) => invocation.args.clone(),
            _ => panic!("one tool binding"),
        };
        assert_eq!(
            args("x"),
            json!({ "count": 3, "to": "done", "deep": { "flag": true }, "list": [1, 2] })
        );
        assert_eq!(args("y"), json!({}));
    }

    #[test]
    fn macros_parse_with_their_steps() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "dave.normal"
            keys = "E"
            macro = "session-explainer"

            [macro.session-explainer]
            label = "card explainer"
            steps = [ { tool = "dave_open_issue" }, { tool = "headway_open_explainer", args = { tab = 1 } } ]
            "#,
        )
        .expect("parse");

        assert_eq!(
            tool_at(&keymap, "dave.normal", "E").as_deref(),
            Some("macro:session-explainer")
        );
        let m = keymap.macro_named("session-explainer").expect("macro");
        assert_eq!(m.label.as_deref(), Some("card explainer"));
        let steps: Vec<_> = m
            .steps
            .iter()
            .map(|s| (s.invocation.name.as_str(), s.invocation.args.clone()))
            .collect();
        assert_eq!(
            steps,
            vec![
                ("dave_open_issue", json!({})),
                ("headway_open_explainer", json!({ "tab": 1 })),
            ]
        );
    }

    #[test]
    fn same_keys_twice_in_a_scope_keep_file_order() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "s"
            keys = "<Esc>"
            tool = "clear_selection"

            [[bind]]
            scope = "s"
            keys = "a"
            tool = "other"

            [[bind]]
            scope = "s"
            keys = "<Esc>"
            tool = "close"
            "#,
        )
        .expect("parse");

        let Match::Exact(bindings) = keymap.lookup("s", keys("<Esc>").as_slice()) else {
            panic!("Esc is bound");
        };
        let names: Vec<_> = bindings
            .iter()
            .map(|b| match &b.action {
                BindAction::Tool(t) => t.name.as_str(),
                BindAction::Macro(_) => unreachable!(),
            })
            .collect();
        assert_eq!(names, ["clear_selection", "close"]);
        assert!(keymap.warnings().is_empty());
    }

    #[test]
    fn a_binding_shadowed_by_its_prefix_is_dropped_with_a_warning() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "s"
            keys = "s e"
            tool = "long"

            [[bind]]
            scope = "s"
            keys = "s"
            tool = "short"

            [[bind]]
            scope = "s"
            keys = "s e x"
            tool = "longer"

            [[bind]]
            scope = "other"
            keys = "s e"
            tool = "fine_here"
            "#,
        )
        .expect("parse");

        assert_eq!(tool_at(&keymap, "s", "s").as_deref(), Some("short"));
        assert!(matches!(
            keymap.lookup("s", keys("s e").as_slice()),
            Match::None
        ));
        assert_eq!(
            tool_at(&keymap, "other", "s e").as_deref(),
            Some("fine_here")
        );
        assert_eq!(keymap.warnings().len(), 2, "{:?}", keymap.warnings());
        assert!(
            keymap.warnings()[0].contains("'s e' can never fire because 's' is bound"),
            "{:?}",
            keymap.warnings()
        );
    }

    /// The error message `src` fails with.
    fn error(src: &str) -> String {
        match parse(src) {
            Ok(_) => panic!("expected an error from:\n{src}"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn bad_bindings_fail_with_a_message_naming_them() {
        let msg = error(
            r#"
            [[bind]]
            scope = "s"
            keys = "j"
            tool = "ok"

            [[bind]]
            scope = "grid"
            keys = "<Ent>"
            tool = "t"
            "#,
        );
        assert!(
            msg.starts_with("bind #2 (scope 'grid', keys '<Ent>'): "),
            "{msg}"
        );
        assert!(msg.contains("'<Ent>' is not a key"), "{msg}");

        let msg = error("[[bind]]\nscope = \"s\"\nkeys = \"j\"\n");
        assert!(msg.contains("needs a 'tool' or a 'macro'"), "{msg}");

        let msg = error(
            "[[bind]]\nscope = \"s\"\nkeys = \"j\"\ntool = \"t\"\nmacro = \"m\"\n\
             [macro.m]\nsteps = [{ tool = \"t\" }]\n",
        );
        assert!(msg.contains("both 'tool' and 'macro'"), "{msg}");

        let msg = error("[[bind]]\nscope = \"s\"\nkeys = \"j\"\nmacro = \"nope\"\n");
        assert!(msg.contains("macro 'nope' is not defined"), "{msg}");

        let msg = error(
            "[[bind]]\nscope = \"s\"\nkeys = \"j\"\nmacro = \"m\"\nargs = { a = 1 }\n\
             [macro.m]\nsteps = [{ tool = \"t\" }]\n",
        );
        assert!(msg.contains("'args' goes on the macro's steps"), "{msg}");

        let msg = error("[macro.m]\nsteps = []\n");
        assert_eq!(msg, "macro 'm' has no steps");
    }

    #[test]
    fn bad_toml_and_unknown_fields_fail() {
        assert!(parse("[[bind]\n").is_err());
        let msg = error("[[bind]]\nscope = \"s\"\nkeys = \"j\"\ntool = \"t\"\nlable = \"x\"\n");
        assert!(msg.contains("lable"), "{msg}");
        let msg = error("[[binds]]\n");
        assert!(msg.contains("binds"), "{msg}");
    }

    #[test]
    fn lookups_take_strokes_read_off_a_press() {
        let keymap =
            parse("[[bind]]\nscope = \"s\"\nkeys = \"?\"\ntool = \"help\"\n").expect("parse");
        let shift_slash = KeyStroke::from_press(notedeck_ui::chord::KeyPress {
            key: egui::Key::Slash,
            modifiers: egui::Modifiers::SHIFT,
        });
        assert!(matches!(
            keymap.lookup("s", &[shift_slash]),
            Match::Exact([_])
        ));
    }

    /// Defaults to merge user keymaps over in the `parse_over` tests.
    const DEFAULTS: &str = r#"
        [[bind]]
        scope = "grid"
        keys = "j"
        tool = "down"
        group = "move"

        [[bind]]
        scope = "grid"
        keys = "<Esc>"
        tool = "clear"

        [[bind]]
        scope = "grid"
        keys = "k"
        tool = "up"
        group = "move"

        [[bind]]
        scope = "grid"
        keys = "<Esc>"
        tool = "close"

        [[bind]]
        scope = "grid"
        keys = "E"
        macro = "explain"

        [macro.explain]
        label = "explain"
        steps = [ { tool = "open" }, { tool = "explain" } ]
    "#;

    /// Every tool [`DEFAULTS`] and the user keymaps below name.
    fn known(tool: &str) -> bool {
        matches!(
            tool,
            "down" | "up" | "clear" | "close" | "open" | "explain" | "first" | "other"
        )
    }

    /// The tools bound to `text` in `scope`, in the order they'd be tried.
    fn tools_at(keymap: &Keymap, scope: &str, text: &str) -> Vec<String> {
        match keymap.lookup(scope, keys(text).as_slice()) {
            Match::Exact(bindings) => bindings
                .iter()
                .map(|b| match &b.action {
                    BindAction::Tool(invocation) => invocation.name.clone(),
                    BindAction::Macro(name) => format!("macro:{name}"),
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The error message merging `user` over [`DEFAULTS`] fails with.
    fn over_error(user: &str) -> String {
        match parse_over(DEFAULTS, user, known) {
            Ok(_) => panic!("expected an error from:\n{user}"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn an_empty_user_keymap_is_the_defaults() {
        let keymap = parse_over(DEFAULTS, "", known).expect("merge");
        assert_eq!(tools_at(&keymap, "grid", "j"), ["down"]);
        assert_eq!(tools_at(&keymap, "grid", "<Esc>"), ["clear", "close"]);
        assert!(keymap.warnings().is_empty());
    }

    #[test]
    fn a_user_binding_replaces_every_default_of_its_keys() {
        let keymap = parse_over(
            DEFAULTS,
            r#"
            [[bind]]
            scope = "grid"
            keys = "j"
            tool = "first"
            group = "move"

            [[bind]]
            scope = "grid"
            keys = "<Esc>"
            tool = "other"

            [[bind]]
            scope = "card"
            keys = "j"
            tool = "down"

            [[bind]]
            scope = "grid"
            keys = "x"
            tool = "other"
            "#,
            known,
        )
        .expect("merge");

        assert_eq!(tools_at(&keymap, "grid", "j"), ["first"]);
        assert_eq!(tools_at(&keymap, "grid", "<Esc>"), ["other"]);
        assert_eq!(tools_at(&keymap, "grid", "k"), ["up"], "untouched");
        assert_eq!(tools_at(&keymap, "card", "j"), ["down"], "a new scope");
        assert_eq!(tools_at(&keymap, "grid", "x"), ["other"], "a new key");

        // The replacement keeps the default's place in its which-key group.
        let grid = keymap.scope("grid").expect("grid");
        let run = grid.group(Some("move")).expect("move group");
        let order: Vec<_> = run
            .members
            .iter()
            .map(|&i| grid.bindings[i].keys.to_string())
            .collect();
        assert_eq!(order, ["j", "k"]);
    }

    #[test]
    fn unbind_removes_a_default_binding() {
        let keymap = parse_over(
            DEFAULTS,
            r#"
            [[bind]]
            scope = "grid"
            keys = "<Esc>"
            unbind = true

            [[bind]]
            scope = "grid"
            keys = "q"
            unbind = true
            "#,
            known,
        )
        .expect("merge");

        assert!(tools_at(&keymap, "grid", "<Esc>").is_empty());
        assert_eq!(tools_at(&keymap, "grid", "j"), ["down"]);
        assert_eq!(
            keymap.warnings(),
            ["in scope 'grid', unbind 'q' matches no default binding"]
        );
    }

    #[test]
    fn a_user_macro_replaces_the_default_of_its_name_or_adds_one() {
        let keymap = parse_over(
            DEFAULTS,
            r#"
            [macro.explain]
            steps = [ { tool = "open" } ]

            [macro.mine]
            label = "mine"
            steps = [ { tool = "first" }, { tool = "down" } ]

            [[bind]]
            scope = "grid"
            keys = "M"
            macro = "mine"
            "#,
            known,
        )
        .expect("merge");

        let explain = keymap.macro_named("explain").expect("explain");
        assert_eq!(explain.steps.len(), 1);
        assert_eq!(explain.label, None, "replaced whole, label too");
        assert_eq!(tools_at(&keymap, "grid", "E"), ["macro:explain"]);
        assert_eq!(tools_at(&keymap, "grid", "M"), ["macro:mine"]);
        assert_eq!(keymap.macro_named("mine").expect("mine").steps.len(), 2);
    }

    #[test]
    fn an_unknown_tool_fails_the_merge() {
        let msg = over_error("[[bind]]\nscope = \"grid\"\nkeys = \"j\"\ntool = \"dwn\"\n");
        assert_eq!(
            msg,
            "bind #1 (scope 'grid', keys 'j'): no app has a tool 'dwn'"
        );

        let msg = over_error("[macro.m]\nsteps = [{ tool = \"open\" }, { tool = \"nope\" }]\n");
        assert_eq!(msg, "macro 'm' step 2: no app has a tool 'nope'");
    }

    #[test]
    fn a_bad_user_keymap_fails_the_merge_whole() {
        assert!(over_error("[[bind]\n").contains("bind"));

        // The second binding is bad, so the first never applies either.
        let msg = over_error(
            "[[bind]]\nscope = \"grid\"\nkeys = \"j\"\ntool = \"up\"\n\
             [[bind]]\nscope = \"grid\"\nkeys = \"<Nope>\"\ntool = \"up\"\n",
        );
        assert!(
            msg.starts_with("bind #2 (scope 'grid', keys '<Nope>'): "),
            "{msg}"
        );

        let msg =
            over_error("[[bind]]\nscope = \"grid\"\nkeys = \"j\"\nunbind = true\ntool = \"up\"\n");
        assert!(msg.contains("'unbind' takes no 'tool' or 'macro'"), "{msg}");

        let msg = over_error("[[bind]]\nscope = \"grid\"\nkeys = \"j\"\nmacro = \"nope\"\n");
        assert!(msg.contains("macro 'nope' is not defined"), "{msg}");
    }

    #[test]
    fn unbind_outside_a_user_keymap_fails() {
        let msg = error("[[bind]]\nscope = \"s\"\nkeys = \"j\"\nunbind = true\n");
        assert!(
            msg.contains("'unbind' only applies to a user keymap"),
            "{msg}"
        );
    }
}
