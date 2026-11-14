//! Portable, backend-agnostic tool *type* layer.
//!
//! An agent tool is advertised to a model in two halves: a **spec** (this
//! module — name, docs, argument schema) and an **execution** (owned by whoever
//! implements the tool). The spec types here are pure data with no egui, no
//! app-host, and no relay dependencies, so they are shared by every consumer of
//! the engine: agentium-core's own dave tool catalog, and — via a re-export —
//! notedeck's browser-level tool module (`notedeck::tool`), which depends on
//! this crate rather than redefining these types. Declaring a tool once here
//! lets a single [`ToolSpec`] build an OpenAI `FunctionObject` or an MCP
//! `tools/list` entry.

use serde_json::Value;

/// The JSON type of a tool argument, as advertised in the tool's schema.
#[derive(Debug, Clone)]
pub enum ToolArgType {
    String,
    Number,
    /// A whole number (JSON Schema `integer`), e.g. a count or a limit.
    Integer,
    /// `true` or `false`.
    Boolean,
    /// A string constrained to a fixed set of values (rendered as JSON `enum`).
    Enum(Vec<&'static str>),
}

impl ToolArgType {
    /// The JSON Schema `type` string for this argument.
    pub fn type_string(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Enum(_) => "string",
        }
    }
}

/// Where an argument goes on a command line, when it isn't a `--flag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Positional {
    /// The next bare word, in the order the spec declares its positionals.
    Slot,
    /// Every bare word left after the slots, joined with spaces: a title or
    /// a comment body typed without quotes. Only the last positional may be
    /// one.
    Rest,
}

/// How an argument reads on a command line (`nd <ns> <verb>`), set with the
/// [`ToolArg`] builders [`positional`](ToolArg::positional),
/// [`rest`](ToolArg::rest), [`short`](ToolArg::short),
/// [`long`](ToolArg::long) and [`env`](ToolArg::env). The default is a
/// `--<name>` flag with no short form and no env fallback.
#[derive(Debug, Clone, Default)]
pub struct ArgCli {
    /// A bare word rather than a flag, when set.
    pub positional: Option<Positional>,
    /// A one-letter `-x` spelling of the flag.
    pub short: Option<char>,
    /// The flag's name when it isn't the argument's, e.g. `--in` for
    /// `container`.
    pub long: Option<&'static str>,
    /// The environment variable read when the argument isn't given.
    pub env: Option<&'static str>,
}

/// One argument in a [`ToolSpec`]: its name, JSON type, docs, and whether it is
/// required. Built with [`ToolArg::new`] plus the [`required`](Self::required)
/// and [`default`](Self::default) builders, and the command-line ones
/// described on [`ArgCli`].
#[derive(Debug, Clone)]
pub struct ToolArg {
    name: &'static str,
    typ: ToolArgType,
    description: &'static str,
    required: bool,
    default: Option<Value>,
    /// Whether it names what the app's view is on (see
    /// [`from_current`](ToolArg::from_current)).
    from_current: bool,
    /// How it reads on a command line.
    cli: ArgCli,
}

impl ToolArg {
    /// A new optional argument with no default. Chain
    /// [`required`](Self::required)/[`default`](Self::default) to refine it.
    pub fn new(name: &'static str, typ: ToolArgType, description: &'static str) -> Self {
        Self {
            name,
            typ,
            description,
            required: false,
            default: None,
            from_current: false,
            cli: ArgCli::default(),
        }
    }

    /// Mark whether the backend must supply this argument.
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Set a default value: the schema's `default`, and appended to the
    /// argument's description so a model that reads only the text sees it.
    pub fn default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self
    }

    /// Mark this argument as naming what the app's view is on, e.g. a card
    /// tool's `card`: a person running the tool from a command bar doesn't
    /// type it, the bar fills it with the app's current ref, as a key
    /// binding's `{current}` does.
    pub fn from_current(mut self) -> Self {
        self.from_current = true;
        self
    }

    /// Take this argument as the next bare word on a command line rather than
    /// a `--flag`. Positionals fill in the order the spec declares them.
    pub fn positional(mut self) -> Self {
        self.cli.positional = Some(Positional::Slot);
        self
    }

    /// Take every bare word left after the other positionals, joined with
    /// spaces, as this argument. Only the last positional may be one.
    pub fn rest(mut self) -> Self {
        self.cli.positional = Some(Positional::Rest);
        self
    }

    /// Give the argument's flag a one-letter `-c` spelling.
    pub fn short(mut self, short: char) -> Self {
        self.cli.short = Some(short);
        self
    }

    /// Spell the argument's flag `--<long>` instead of `--<name>`, when the
    /// name a model sees reads badly typed, e.g. `--in` for `container`.
    pub fn long(mut self, long: &'static str) -> Self {
        self.cli.long = Some(long);
        self
    }

    /// Fill the argument from the environment variable `var` when a command
    /// line leaves it out. The variable is the caller's, so the client reads
    /// it; the tool never does.
    pub fn env(mut self, var: &'static str) -> Self {
        self.cli.env = Some(var);
        self
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// How it reads on a command line.
    pub fn cli(&self) -> &ArgCli {
        &self.cli
    }

    /// Its JSON type.
    pub fn typ(&self) -> &ToolArgType {
        &self.typ
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    /// Whether the caller must supply it.
    pub fn is_required(&self) -> bool {
        self.required
    }

    /// The value it takes when left out, if it has one.
    pub fn default_value(&self) -> Option<&Value> {
        self.default.as_ref()
    }

    /// Whether it names what the app's view is on (see
    /// [`from_current`](Self::from_current)).
    pub fn is_from_current(&self) -> bool {
        self.from_current
    }
}

/// Which side of the host a tool runs on.
///
/// Both kinds share one name space and one spec type, so a backend lists them
/// together and tells them apart by [`ToolSpec::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Runs over shared, nostr-backed data and returns JSON for the caller. The
    /// kind an agent backend advertises to its model.
    Data,
    /// Runs against one app's own in-memory UI state (move a cursor, open a
    /// pane), dispatched to that app by the shell. Bound to keys and macros;
    /// agent backends skip these unless they opt in.
    Ui,
}

/// A portable, backend-agnostic description of a tool: what it's called, what it
/// does, and the arguments it accepts. Reused to build an OpenAI
/// `FunctionObject` (via [`json_schema`](Self::json_schema)) or an MCP
/// `tools/list` entry, so a tool is declared exactly once.
///
/// A tool a person runs by hand (from a command bar, a menu) also carries a
/// human [`title`](Self::title) and a [`category`](Self::category) to list it
/// under. Neither reaches a model's schema.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    name: &'static str,
    description: &'static str,
    arguments: Vec<ToolArg>,
    kind: ToolKind,
    /// What a person calls it, e.g. "Open issue list"; `None` until set.
    title: Option<&'static str>,
    /// What it's listed under, e.g. "Headway"; `None` until set.
    category: Option<&'static str>,
    /// Whether it only answers, changing nothing (see
    /// [`read_only`](ToolSpec::read_only)).
    read_only: bool,
    /// The command line that runs it, e.g. `["headway", "show"]`; empty for
    /// none (see [`command`](ToolSpec::command)).
    command: &'static [&'static str],
}

impl ToolSpec {
    /// Declare a [`ToolKind::Data`] tool with the given name, description, and
    /// arguments. Chain [`ui`](Self::ui) for a UI tool.
    pub fn new(name: &'static str, description: &'static str, arguments: Vec<ToolArg>) -> Self {
        Self {
            name,
            description,
            arguments,
            kind: ToolKind::Data,
            title: None,
            category: None,
            read_only: false,
            command: &[],
        }
    }

    /// Mark this spec as a [`ToolKind::Ui`] tool.
    pub fn ui(mut self) -> Self {
        self.kind = ToolKind::Ui;
        self
    }

    /// Give the tool a human title, a short imperative phrase in sentence
    /// case: "Open issue list", "Toggle key hints".
    pub fn titled(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// File the tool under `category`, the name of the app or part of the
    /// shell it belongs to: "Headway", "Chrome".
    pub fn filed_under(mut self, category: &'static str) -> Self {
        self.category = Some(category);
        self
    }

    /// File the tool under `category` unless it is filed already, so an app
    /// can stamp its name on all its specs while one of them files itself
    /// elsewhere.
    pub fn default_category(self, category: &'static str) -> Self {
        match self.category {
            Some(_) => self,
            None => self.filed_under(category),
        }
    }

    /// Mark the tool as one that only answers, changing nothing: a query
    /// like "Show card", whose JSON is for an agent or a macro step. MCP's
    /// `readOnlyHint`. A command bar, which would throw the answer away,
    /// doesn't list it.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Give the tool a command line, a namespace then a verb:
    /// `["headway", "show"]` is `nd headway show`. Two tools may share one;
    /// the client tells them apart by how many positionals each requires.
    /// A tool without one is still reachable as `nd call <name>`.
    pub fn command(mut self, path: &'static [&'static str]) -> Self {
        self.command = path;
        self
    }

    /// Its command line (see [`command`](Self::command)), empty when it has
    /// none.
    pub fn command_path(&self) -> &'static [&'static str] {
        self.command
    }

    /// Whether it only answers, changing nothing (see
    /// [`read_only`](Self::read_only)).
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Whether this tool runs over data or against an app's UI state.
    pub fn kind(&self) -> ToolKind {
        self.kind
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn arguments(&self) -> &[ToolArg] {
        &self.arguments
    }

    /// Its human title, set with [`titled`](Self::titled).
    pub fn title(&self) -> Option<&'static str> {
        self.title
    }

    /// What to show a person: its [title](Self::title), else its registered
    /// name.
    pub fn display_title(&self) -> &'static str {
        self.title.unwrap_or(self.name)
    }

    /// What it's filed under, set with [`filed_under`](Self::filed_under).
    pub fn category(&self) -> Option<&'static str> {
        self.category
    }

    /// Whether it can only run on what an app's view is on: a required
    /// argument is [from the current ref](ToolArg::from_current).
    pub fn needs_current(&self) -> bool {
        self.arguments
            .iter()
            .any(|arg| arg.required && arg.from_current)
    }

    /// The JSON Schema object describing this tool's arguments — the `parameters`
    /// payload for an OpenAI `FunctionObject` and the `inputSchema` for an MCP
    /// tool entry. Required args are listed in `required`; each property carries
    /// its type, description (with any default appended), `default`, and `enum`
    /// constraint.
    ///
    /// A tool with a [`command`](Self::command) also carries what a command
    /// line needs, as `x-nd-` keys a model ignores:
    ///
    /// - `x-nd-order`, on the object: the argument names in declaration order.
    ///   A client parsing `properties` into a sorted map (`serde_json` without
    ///   `preserve_order`, as `nd` builds) would otherwise read positionals
    ///   alphabetically.
    /// - On a property: `x-nd-positional` (its slot index, or `"rest"`),
    ///   `x-nd-short` (one letter), `x-nd-long` (the flag's name) and
    ///   `x-nd-env` (the variable to fall back to).
    pub fn json_schema(&self) -> Value {
        let required_args: Vec<Value> = self
            .arguments
            .iter()
            .filter(|arg| arg.required)
            .map(|arg| Value::String(arg.name.to_owned()))
            .collect();

        let mut properties = serde_json::Map::new();
        let mut slots = 0;
        for arg in &self.arguments {
            let mut props = serde_json::Map::new();
            props.insert(
                "type".to_string(),
                Value::String(arg.typ.type_string().to_string()),
            );

            let description = if let Some(default) = &arg.default {
                format!("{} (Default: {default})", arg.description)
            } else {
                arg.description.to_owned()
            };
            props.insert("description".to_string(), Value::String(description));

            if let Some(default) = &arg.default {
                props.insert("default".to_string(), default.clone());
            }

            if let ToolArgType::Enum(variants) = &arg.typ {
                props.insert(
                    "enum".to_string(),
                    Value::Array(
                        variants
                            .iter()
                            .map(|s| Value::String((*s).to_owned()))
                            .collect(),
                    ),
                );
            }

            if !self.command.is_empty() {
                insert_cli_keys(&mut props, &arg.cli, &mut slots);
            }

            properties.insert(arg.name.to_owned(), Value::Object(props));
        }

        let mut parameters = serde_json::Map::new();
        parameters.insert("type".to_string(), Value::String("object".to_string()));
        parameters.insert("required".to_string(), Value::Array(required_args));
        parameters.insert("additionalProperties".to_string(), Value::Bool(false));
        parameters.insert("properties".to_string(), Value::Object(properties));
        if !self.command.is_empty() {
            let order = self
                .arguments
                .iter()
                .map(|arg| Value::String(arg.name.to_owned()))
                .collect();
            parameters.insert("x-nd-order".to_string(), Value::Array(order));
        }

        Value::Object(parameters)
    }
}

/// Write `cli`'s `x-nd-` keys onto one property of
/// [`ToolSpec::json_schema`]. `slots` counts the positional slots handed out
/// so far, so each gets the next index.
fn insert_cli_keys(props: &mut serde_json::Map<String, Value>, cli: &ArgCli, slots: &mut u64) {
    match cli.positional {
        Some(Positional::Slot) => {
            props.insert("x-nd-positional".to_string(), Value::from(*slots));
            *slots += 1;
        }
        Some(Positional::Rest) => {
            props.insert("x-nd-positional".to_string(), Value::from("rest"));
        }
        None => {}
    }
    if let Some(short) = cli.short {
        props.insert("x-nd-short".to_string(), Value::String(short.to_string()));
    }
    if let Some(long) = cli.long {
        props.insert("x-nd-long".to_string(), Value::from(long));
    }
    if let Some(env) = cli.env {
        props.insert("x-nd-env".to_string(), Value::from(env));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A tool with a command line writes its argument order and each
    /// argument's command-line keys; a default lands as `default` beside the
    /// description text that still carries it.
    #[test]
    fn a_command_tool_writes_its_cli_keys() {
        let spec = ToolSpec::new(
            "demo_add",
            "Add a thing.",
            vec![
                ToolArg::new("card", ToolArgType::String, "The card.")
                    .required(true)
                    .positional(),
                ToolArg::new("title", ToolArgType::String, "The title.").rest(),
                ToolArg::new("container", ToolArgType::String, "Where.").long("in"),
                ToolArg::new("limit", ToolArgType::Integer, "How many.")
                    .short('n')
                    .default(json!(5)),
                ToolArg::new("board", ToolArgType::String, "The board.").env("DEMO_BOARD"),
            ],
        )
        .command(&["demo", "add"]);
        let schema = spec.json_schema();

        assert_eq!(
            schema["x-nd-order"],
            json!(["card", "title", "container", "limit", "board"])
        );
        let props = &schema["properties"];
        assert_eq!(props["card"]["x-nd-positional"], json!(0));
        assert_eq!(props["title"]["x-nd-positional"], json!("rest"));
        assert_eq!(props["container"]["x-nd-long"], json!("in"));
        assert_eq!(props["limit"]["type"], json!("integer"));
        assert_eq!(props["limit"]["x-nd-short"], json!("n"));
        assert_eq!(props["limit"]["default"], json!(5));
        assert_eq!(
            props["limit"]["description"],
            json!("How many. (Default: 5)")
        );
        assert_eq!(props["board"]["x-nd-env"], json!("DEMO_BOARD"));
        assert!(props["board"].get("x-nd-positional").is_none());
        assert_eq!(spec.command_path(), ["demo", "add"]);
    }

    /// A tool with no command line keeps a model's schema free of `x-nd-`
    /// keys, even on arguments that declare them.
    #[test]
    fn a_tool_without_a_command_writes_no_cli_keys() {
        let spec = ToolSpec::new(
            "demo_show",
            "Show a thing.",
            vec![ToolArg::new("card", ToolArgType::String, "The card.").positional()],
        );
        let schema = spec.json_schema().to_string();
        assert!(!schema.contains("x-nd-"), "{schema}");
    }
}
