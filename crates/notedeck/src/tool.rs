//! Browser-level, statically-typed agent tools.
//!
//! An "agent tool" is a capability an AI backend can invoke — most importantly
//! a full-text nostrdb query. These are *browser*-owned: they operate on shared
//! state the host holds (the db, caches, accounts), not on any one app's private
//! state, so notedeck models them as a single statically-typed [`ToolCall`]
//! enum rather than a bag of dynamically-dispatched handlers. A dynamic
//! per-app handler could only ever touch this same browser state (it never sees
//! the app's `&mut self`), so it would buy indirection without capability; an
//! app that wants a tool over its own state handles that in its own update loop.
//!
//! A tool is described in two portable halves so one declaration serves every
//! backend:
//!
//! - the **spec** ([`ToolSpec`]) — a backend-agnostic description (name, docs,
//!   argument schema) reused to build an OpenAI `FunctionObject` or an MCP
//!   `tools/list` entry, and
//! - the **execution** ([`ToolCall::call`]) — runs the parsed, typed call
//!   against a [`ToolContext`] and returns a [`ToolOutcome`].
//!
//! A backend advertises [`ToolCall::specs`], parses an incoming call with
//! [`ToolCall::parse`], and dispatches it with [`ToolCall::call`] over a
//! [`ToolContext`] built from an [`AppContext`](crate::AppContext) via
//! [`tool_context`](crate::AppContext::tool_context) — the tool analogue of
//! `note_context()`.

use enostr::Pubkey;
use nostrdb::{Filter, Ndb, Note, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Accounts, AppAction, NoteCache};

/// The JSON type of a tool argument, as advertised in the tool's schema.
#[derive(Debug, Clone)]
pub enum ToolArgType {
    String,
    Number,
    /// A string constrained to a fixed set of values (rendered as JSON `enum`).
    Enum(Vec<&'static str>),
}

impl ToolArgType {
    /// The JSON Schema `type` string for this argument.
    pub fn type_string(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
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

/// A portable, backend-agnostic description of a tool: what it's called, what it
/// does, and the arguments it accepts. Reused to build an OpenAI
/// `FunctionObject` (via [`json_schema`](Self::json_schema)) or an MCP
/// `tools/list` entry, so a tool is declared exactly once.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    name: &'static str,
    description: &'static str,
    arguments: Vec<ToolArg>,
}

impl ToolSpec {
    /// Declare a tool with the given name, description, and arguments.
    pub fn new(name: &'static str, description: &'static str, arguments: Vec<ToolArg>) -> Self {
        Self {
            name,
            description,
            arguments,
        }
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

/// The browser state a tool executes against, reborrowed from an
/// [`AppContext`](crate::AppContext) via
/// [`tool_context`](crate::AppContext::tool_context) — the tool analogue of
/// [`NoteContext`](crate::NoteContext). Tools open their own short-lived
/// [`Transaction`]s from [`ndb`](Self::ndb) as needed.
pub struct ToolContext<'a> {
    pub ndb: &'a Ndb,
    pub note_cache: &'a mut NoteCache,
    pub accounts: &'a Accounts,
}

/// The result of running a [`ToolCall`].
///
/// [`Data`](Self::Data) is a pure JSON value with no UI side effects — safe to
/// hand straight back to the backend. [`Intent`](Self::Intent) is a UI side
/// effect the caller marshals as a next-frame [`AppAction`] (e.g. routing a
/// column). [`Error`](Self::Error) reports a failure to relay to the backend.
pub enum ToolOutcome {
    Data(Value),
    Intent(AppAction),
    Error(String),
}

/// A statically-typed, browser-provided tool call, parsed from a backend's
/// request. Each variant owns a concrete argument struct (so a query still calls
/// [`QueryCall::to_filter`] on a real type), and the whole set is what a backend
/// advertises via [`specs`](Self::specs) and dispatches via [`call`](Self::call).
#[derive(Debug, Clone)]
pub enum ToolCall {
    /// A full-text / filtered nostrdb query.
    Query(QueryCall),
}

impl ToolCall {
    /// The specs for every browser-provided tool — what a backend advertises
    /// (OpenAI `tools`, MCP `tools/list`).
    pub fn specs() -> Vec<ToolSpec> {
        vec![QueryCall::spec()]
    }

    /// The registered name of this tool call.
    pub fn name(&self) -> &'static str {
        match self {
            ToolCall::Query(_) => QueryCall::NAME,
        }
    }

    /// Parse a backend's tool call by `name` and raw JSON `args` into a typed
    /// [`ToolCall`]. Returns a human-readable error for an unknown tool or
    /// arguments that don't fit the tool's schema.
    pub fn parse(name: &str, args: &Value) -> Result<ToolCall, String> {
        match name {
            QueryCall::NAME => serde_json::from_value::<QueryCall>(args.clone())
                .map(ToolCall::Query)
                .map_err(|e| format!("failed to parse '{name}' arguments: {e}")),
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    /// Execute the call against `cx`, returning what to hand back to the backend
    /// or apply to the UI.
    pub fn call(self, cx: &mut ToolContext) -> ToolOutcome {
        match self {
            ToolCall::Query(query) => {
                let txn = match Transaction::new(cx.ndb) {
                    Ok(txn) => txn,
                    Err(err) => return ToolOutcome::Error(format!("failed to open db: {err}")),
                };
                ToolOutcome::Data(json!({ "note_ids": query.execute(&txn, cx.ndb) }))
            }
        }
    }
}

/// Whether a note is a reply (used to exclude replies from query results, so
/// searches surface root notes rather than conversation fragments).
fn is_reply(note: Note) -> bool {
    for tag in note.tags() {
        if tag.count() < 4 {
            continue;
        }

        let Some("e") = tag.get_str(0) else {
            continue;
        };

        let Some(s) = tag.get_str(3) else {
            continue;
        };

        if s == "root" || s == "reply" {
            return true;
        }
    }

    false
}

/// A parsed nostrdb query the backend wants to run: an optional full-text
/// `search`, constrained by author, kind, time range, and a result limit.
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct QueryCall {
    pub author: Option<Pubkey>,
    pub limit: Option<u64>,
    pub since: Option<u64>,
    pub kind: Option<u64>,
    pub until: Option<u64>,
    pub search: Option<String>,
}

impl QueryCall {
    /// The tool name advertised to backends.
    pub const NAME: &'static str = "query";

    /// The portable spec advertised for this tool.
    pub fn spec() -> ToolSpec {
        ToolSpec::new(
            Self::NAME,
            "Note query functionality. Used for finding notes using full-text search terms, scoped by different contexts. You can use a combination of limit, since, and until to pull notes from any time range.",
            vec![
                ToolArg::new(
                    "search",
                    ToolArgType::String,
                    "A fulltext search query. Queries with multiple words will only return results with notes that have all of those words. Don't include filler words/symbols like 'and', punctuation, etc",
                ),
                ToolArg::new("limit", ToolArgType::Number, "The number of results to return.")
                    .required(true)
                    .default(Value::from(50)),
                ToolArg::new(
                    "since",
                    ToolArgType::Number,
                    "Only pull notes after this unix timestamp",
                ),
                ToolArg::new(
                    "until",
                    ToolArgType::Number,
                    "Only pull notes up until this unix timestamp. Always include this when searching notes within some date range (yesterday, last week, etc).",
                ),
                ToolArg::new(
                    "author",
                    ToolArgType::String,
                    "An author *pubkey* to constrain the query on. Can be used to search for notes from individual users. If unsure what pubkey to use, you can query for kind 0 profiles with the search argument.",
                ),
                ToolArg::new("kind", ToolArgType::Number, r#"The kind of note. Kind list:
                - 0: profiles
                - 1: microblogs/\"tweets\"/posts
                - 6: reposts of kind 1 notes
                - 7: emoji reactions/likes
                - 9735: zaps (bitcoin micropayment receipts)
                - 30023: longform articles, blog posts, etc
                "#)
                    .default(Value::from(1)),
            ],
        )
    }

    /// Build the nostrdb [`Filter`] this query describes. Replies are excluded so
    /// results are root notes rather than conversation fragments.
    pub fn to_filter(&self) -> Filter {
        let mut filter = Filter::new()
            .limit(self.limit())
            .custom(|n| !is_reply(n))
            .kinds([self.kind.unwrap_or(1)]);

        if let Some(author) = &self.author {
            filter = filter.authors([author.bytes()]);
        }

        if let Some(search) = &self.search {
            filter = filter.search(search);
        }

        if let Some(until) = self.until {
            filter = filter.until(until);
        }

        if let Some(since) = self.since {
            filter = filter.since(since);
        }

        filter.build()
    }

    /// The result limit, defaulting to 10 when the backend omits it.
    pub fn limit(&self) -> u64 {
        self.limit.unwrap_or(10)
    }

    /// Run the query, returning the hex note ids of the matches (stable nostr
    /// ids, so the result is portable to any backend).
    pub fn execute(&self, txn: &Transaction, ndb: &Ndb) -> Vec<String> {
        ndb.query(txn, &[self.to_filter()], self.limit() as i32)
            .map(|results| {
                results
                    .into_iter()
                    .map(|r| hex::encode(r.note.id()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_spec_json_schema_shape() {
        let schema = QueryCall::spec().json_schema();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], Value::Bool(false));
        // Only `limit` is required.
        assert_eq!(schema["required"], Value::Array(vec![Value::from("limit")]));

        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props["search"]["type"], "string");
        assert_eq!(props["limit"]["type"], "number");
        // The default is folded into the description the model sees, with no
        // double-paren regression.
        let limit_desc = props["limit"]["description"].as_str().unwrap();
        assert!(limit_desc.contains("Default: 50"));
        assert!(!limit_desc.contains("))"));
    }

    #[test]
    fn parse_query_call() {
        let args = json!({ "search": "nostr", "limit": 5 });
        let ToolCall::Query(q) = ToolCall::parse("query", &args).unwrap();
        assert_eq!(q.search.as_deref(), Some("nostr"));
        assert_eq!(q.limit, Some(5));
    }

    #[test]
    fn parse_unknown_tool_errors() {
        let err = ToolCall::parse("nope", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[test]
    fn query_defaults() {
        // An empty query is valid and falls back to sensible defaults.
        let ToolCall::Query(q) = ToolCall::parse("query", &json!({})).unwrap();
        assert_eq!(q.limit(), 10);
        assert_eq!(q.kind, None);
    }
}
