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
            Self::Boolean => "boolean",
            Self::Enum(_) => "string",
        }
    }
}

/// One argument in a [`ToolSpec`]: its name, JSON type, docs, and whether it is
/// required. Built with [`ToolArg::new`] plus the [`required`](Self::required)
/// and [`default`](Self::default) builders.
#[derive(Debug, Clone)]
pub struct ToolArg {
    name: &'static str,
    typ: ToolArgType,
    description: &'static str,
    required: bool,
    default: Option<Value>,
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
        }
    }

    /// Mark whether the backend must supply this argument.
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Set a default value, appended to the argument's description in the schema
    /// so the model sees it.
    pub fn default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self
    }

    pub fn name(&self) -> &'static str {
        self.name
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
#[derive(Debug, Clone)]
pub struct ToolSpec {
    name: &'static str,
    description: &'static str,
    arguments: Vec<ToolArg>,
    kind: ToolKind,
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
        }
    }

    /// Mark this spec as a [`ToolKind::Ui`] tool.
    pub fn ui(mut self) -> Self {
        self.kind = ToolKind::Ui;
        self
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

    /// The JSON Schema object describing this tool's arguments — the `parameters`
    /// payload for an OpenAI `FunctionObject` and the `inputSchema` for an MCP
    /// tool entry. Required args are listed in `required`; each property carries
    /// its type, description (with any default appended), and `enum` constraint.
    pub fn json_schema(&self) -> Value {
        let required_args: Vec<Value> = self
            .arguments
            .iter()
            .filter(|arg| arg.required)
            .map(|arg| Value::String(arg.name.to_owned()))
            .collect();

        let mut properties = serde_json::Map::new();
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

            properties.insert(arg.name.to_owned(), Value::Object(props));
        }

        let mut parameters = serde_json::Map::new();
        parameters.insert("type".to_string(), Value::String("object".to_string()));
        parameters.insert("required".to_string(), Value::Array(required_args));
        parameters.insert("additionalProperties".to_string(), Value::Bool(false));
        parameters.insert("properties".to_string(), Value::Object(properties));

        Value::Object(parameters)
    }
}
