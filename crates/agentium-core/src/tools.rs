use crate::messages::ExecutedTool;
use crate::tool::{ToolArg, ToolArgType, ToolSpec};
use chrono::DateTime;
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use nostrdb_net::{NoteId, Pubkey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, fmt};

/// A tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    typ: ToolCalls,
}

impl ToolCall {
    pub fn new(id: String, typ: ToolCalls) -> Self {
        Self { id, typ }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn invalid(
        id: String,
        name: Option<String>,
        arguments: Option<String>,
        error: String,
    ) -> Self {
        Self {
            id,
            typ: ToolCalls::Invalid(InvalidToolCall {
                name,
                arguments,
                error,
            }),
        }
    }

    pub fn calls(&self) -> &ToolCalls {
        &self.typ
    }
}

/// On streaming APIs, tool calls are incremental. We use this
/// to represent tool calls that are in the process of returning.
/// These eventually just become [`ToolCall`]'s
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct PartialToolCall {
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

impl PartialToolCall {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn id_mut(&mut self) -> &mut Option<String> {
        &mut self.id
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn name_mut(&mut self) -> &mut Option<String> {
        &mut self.name
    }

    pub fn arguments(&self) -> Option<&str> {
        self.arguments.as_deref()
    }

    pub fn arguments_mut(&mut self) -> &mut Option<String> {
        &mut self.arguments
    }
}

/// The query response from nostrdb for a given context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResponse {
    pub notes: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Error(String),
    Query(QueryResponse),
    PresentNotes(i32),
    ExecutedTool(ExecutedTool),
    /// What an app tool returned (see [`ToolCalls::App`]).
    App(AppToolResponse),
}

/// An app tool's answer: the JSON value it returned, handed to the model as
/// is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppToolResponse {
    /// The tool's registered name, e.g. `headway_show_board`.
    pub tool: String,
    /// What it returned.
    pub value: Value,
}

#[derive(Debug, Clone)]
pub struct UnknownToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl UnknownToolCall {
    pub fn parse(&self, tools: &HashMap<String, Tool>) -> Result<ToolCall, ToolCallError> {
        let Some(tool) = tools.get(&self.name) else {
            return Err(ToolCallError::NotFound(self.name.to_owned()));
        };

        let parsed_args = match tool.parse_call {
            ToolParse::Typed(parse) => parse(&self.arguments)?,
            ToolParse::App => ToolCalls::App(AppToolCall::parse(tool.name(), &self.arguments)?),
        };
        Ok(ToolCall {
            id: self.id.clone(),
            typ: parsed_args,
        })
    }
}

impl PartialToolCall {
    pub fn complete(&self) -> Option<UnknownToolCall> {
        Some(UnknownToolCall {
            id: self.id.clone()?,
            name: self.name.clone()?,
            arguments: self.arguments.clone()?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidToolCall {
    pub error: String,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// An enumeration of the possible tool calls that
/// can be parsed from Dave responses. When adding
/// new tools, this needs to be updated so that we can
/// handle tool call responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolCalls {
    Query(QueryCall),
    PresentNotes(PresentNotesCall),
    Invalid(InvalidToolCall),
    /// A call of an app's tool (a headway or notebook tool), which Dave runs
    /// through the host's tool registry rather than a parser of its own.
    App(AppToolCall),
}

/// A call of an app tool: its name and the model's arguments, untyped. The
/// registry checks the arguments against the tool's own types when it runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppToolCall {
    /// The tool's registered name, e.g. `headway_show_board`.
    pub name: String,
    /// The model's arguments.
    pub args: Value,
}

impl AppToolCall {
    /// Read the model's raw `arguments` for the tool `name`: a JSON object,
    /// or nothing for a tool that takes no arguments.
    fn parse(name: &str, arguments: &str) -> Result<Self, ToolCallError> {
        let args = if arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str::<Value>(arguments)
                .map_err(|e| ToolCallError::ArgParseFailure(format!("{arguments}, error: {e}")))?
        };
        if !args.is_object() {
            return Err(ToolCallError::ArgParseFailure(format!(
                "{arguments}: the arguments must be a JSON object"
            )));
        }
        Ok(Self {
            name: name.to_owned(),
            args,
        })
    }
}

impl ToolCalls {
    /// The function name used in the backend API tool-call schema
    /// (distinct from [`tool_name`](Self::tool_name), the registry name).
    /// An app tool has one name for both.
    pub fn api_name(&self) -> &str {
        match self {
            Self::Query(_) => "search",
            Self::Invalid(_) => "error",
            Self::PresentNotes(_) => "present",
            Self::App(call) => &call.name,
        }
    }

    /// Returns the tool name as defined in the tool registry (for prompt-based tool calls)
    pub fn tool_name(&self) -> &str {
        match self {
            Self::Query(_) => "query",
            Self::Invalid(_) => "invalid",
            Self::PresentNotes(_) => "present_notes",
            Self::App(call) => &call.name,
        }
    }

    pub fn arguments(&self) -> String {
        match self {
            Self::Query(search) => serde_json::to_string(search).unwrap(),
            Self::Invalid(partial) => serde_json::to_string(partial).unwrap(),
            Self::PresentNotes(call) => serde_json::to_string(&call.to_simple()).unwrap(),
            Self::App(call) => call.args.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum ToolCallError {
    EmptyName,
    EmptyArgs,
    NotFound(String),
    ArgParseFailure(String),
}

impl fmt::Display for ToolCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolCallError::EmptyName => write!(f, "the tool name was empty"),
            ToolCallError::EmptyArgs => write!(f, "no arguments were provided"),
            ToolCallError::NotFound(ref name) => write!(f, "tool '{name}' not found"),
            ToolCallError::ArgParseFailure(ref msg) => {
                write!(f, "failed to parse arguments: {msg}")
            }
        }
    }
}

/// A dave tool: the portable [`ToolSpec`] advertised to the model plus dave's
/// parser that turns the model's raw arguments into a typed [`ToolCalls`] for
/// dave's streaming, persistence, and UI layers.
#[derive(Debug, Clone)]
pub struct Tool {
    parse_call: ToolParse,
    spec: ToolSpec,
}

/// How a [`Tool`]'s raw arguments become a [`ToolCalls`].
#[derive(Debug, Clone, Copy)]
enum ToolParse {
    /// One of dave's own tools, with its typed parser.
    Typed(fn(&str) -> Result<ToolCalls, ToolCallError>),
    /// An app tool: the arguments stay JSON (see [`ToolCalls::App`]).
    App,
}

impl Tool {
    /// An app tool offered to the model under its own spec. Its calls parse
    /// to [`ToolCalls::App`], for the host's tool registry to run.
    pub fn app(spec: ToolSpec) -> Self {
        Self {
            parse_call: ToolParse::App,
            spec,
        }
    }

    pub fn name(&self) -> &'static str {
        self.spec.name()
    }

    pub fn description(&self) -> &'static str {
        self.spec.description()
    }

    /// The portable spec advertised to backends. Backends build whatever
    /// function/tool envelope their API expects from [`ToolSpec::json_schema`]
    /// (e.g. an OpenAI `FunctionObject`).
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }
}

impl ToolResponses {
    pub fn format_for_dave(&self, txn: &Transaction, ndb: &Ndb) -> String {
        format_tool_response_for_ai(txn, ndb, self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    id: String,
    typ: ToolResponses,
}

impl ToolResponse {
    pub fn new(id: String, responses: ToolResponses) -> Self {
        Self { id, typ: responses }
    }

    pub fn error(id: String, msg: String) -> Self {
        Self {
            id,
            typ: ToolResponses::Error(msg),
        }
    }

    pub fn executed_tool(result: ExecutedTool) -> Self {
        Self {
            id: String::new(),
            typ: ToolResponses::ExecutedTool(result),
        }
    }

    pub fn responses(&self) -> &ToolResponses {
        &self.typ
    }

    /// Mutable access to the response, for filling in detail a reloaded row
    /// lost on the wire (see dave's reconcile at rest).
    pub fn responses_mut(&mut self) -> &mut ToolResponses {
        &mut self.typ
    }

    /// Take the response out of the row.
    pub fn into_responses(self) -> ToolResponses {
        self.typ
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Called by dave when he wants to display notes on the screen
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PresentNotesCall {
    pub note_ids: Vec<NoteId>,
}

impl PresentNotesCall {
    fn to_simple(&self) -> PresentNotesCallSimple {
        let note_ids = self
            .note_ids
            .iter()
            .map(|nid| hex::encode(nid.bytes()))
            .collect::<Vec<_>>()
            .join(",");

        PresentNotesCallSimple { note_ids }
    }
}

/// Called by dave when he wants to display notes on the screen
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PresentNotesCallSimple {
    note_ids: String,
}

impl PresentNotesCall {
    fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        match serde_json::from_str::<PresentNotesCallSimple>(args) {
            Ok(call) => {
                let note_ids = call
                    .note_ids
                    .split(",")
                    .filter_map(|n| NoteId::from_hex(n).ok())
                    .collect();

                Ok(ToolCalls::PresentNotes(PresentNotesCall { note_ids }))
            }
            Err(e) => Err(ToolCallError::ArgParseFailure(format!(
                "{args}, error: {e}"
            ))),
        }
    }
}

/// The parsed nostrdb query a backend wants to run to satisfy a request: an
/// optional full-text `search`, constrained by author, kind, time range, and a
/// result limit. Executes to local nostrdb note keys ([`QueryResponse`]).
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct QueryCall {
    pub author: Option<Pubkey>,
    pub limit: Option<u64>,
    pub since: Option<u64>,
    pub kind: Option<u64>,
    pub until: Option<u64>,
    pub search: Option<String>,
}

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
                ToolArg::new(
                    "kind",
                    ToolArgType::Number,
                    r#"The kind of note. Kind list:
                - 0: profiles
                - 1: microblogs/\"tweets\"/posts
                - 6: reposts of kind 1 notes
                - 7: emoji reactions/likes
                - 9735: zaps (bitcoin micropayment receipts)
                - 30023: longform articles, blog posts, etc
                "#,
                )
                .default(Value::from(1)),
            ],
        )
    }

    pub fn to_filter(&self) -> nostrdb::Filter {
        let mut filter = nostrdb::Filter::new()
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

    pub fn author(&self) -> Option<&Pubkey> {
        self.author.as_ref()
    }

    pub fn since(&self) -> Option<u64> {
        self.since
    }

    pub fn until(&self) -> Option<u64> {
        self.until
    }

    pub fn search(&self) -> Option<&str> {
        self.search.as_deref()
    }

    pub fn execute(&self, txn: &Transaction, ndb: &Ndb) -> QueryResponse {
        let notes = {
            if let Ok(results) = ndb.query(txn, &[self.to_filter()], self.limit() as i32) {
                results.into_iter().map(|r| r.note_key.as_u64()).collect()
            } else {
                vec![]
            }
        };
        QueryResponse { notes }
    }

    pub fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        match serde_json::from_str::<QueryCall>(args) {
            Ok(call) => Ok(ToolCalls::Query(call)),
            Err(e) => Err(ToolCallError::ArgParseFailure(format!(
                "{args}, error: {e}"
            ))),
        }
    }
}

/// A simple note format for use when formatting
/// tool responses and thread summaries
#[derive(Debug, Serialize)]
pub struct SimpleNote {
    pub note_id: String,
    pub pubkey: String,
    pub name: String,
    pub content: String,
    pub created_at: String,
    pub note_kind: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// Convert a note to a SimpleNote for AI consumption.
pub fn note_to_simple(txn: &Transaction, ndb: &Ndb, note: &Note<'_>) -> SimpleNote {
    let name = ndb
        .get_profile_by_pubkey(txn, note.pubkey())
        .ok()
        .and_then(|p| p.record().profile())
        .and_then(|p| p.name().or_else(|| p.display_name()))
        .unwrap_or("Anonymous")
        .to_string();

    let created_at = DateTime::from_timestamp(note.created_at() as i64, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let reply_to = nostrdb::NoteReply::new(note.tags())
        .reply()
        .map(|r| hex::encode(r.id));

    SimpleNote {
        note_id: hex::encode(note.id()),
        pubkey: hex::encode(note.pubkey()),
        name,
        content: note.content().to_owned(),
        created_at,
        note_kind: note.kind() as u64,
        reply_to,
    }
}

/// Format a list of SimpleNotes as JSON for AI consumption.
pub fn format_simple_notes_json(notes: &[SimpleNote]) -> String {
    serde_json::to_string(&json!({"thread": notes})).unwrap()
}

/// Take the result of a tool response and present it to the ai so that
/// it can interepret it and take further action
fn format_tool_response_for_ai(txn: &Transaction, ndb: &Ndb, resp: &ToolResponses) -> String {
    match resp {
        ToolResponses::PresentNotes(n) => format!("{n} notes presented to the user"),
        ToolResponses::Error(s) => format!("error: {}", s),

        ToolResponses::Query(search_r) => {
            let simple_notes: Vec<SimpleNote> = search_r
                .notes
                .iter()
                .filter_map(|nkey| {
                    let note = ndb.get_note_by_key(txn, NoteKey::new(*nkey)).ok()?;
                    Some(note_to_simple(txn, ndb, &note))
                })
                .collect();

            serde_json::to_string(&json!({"search_results": simple_notes})).unwrap()
        }

        ToolResponses::ExecutedTool(r) => format!("{}: {}", r.tool_name, r.summary),

        // A string answer (a coding tool's file text or command output) is
        // the text itself: JSON-quoting it would escape every newline and
        // quote the model has to read through.
        ToolResponses::App(r) => match &r.value {
            Value::String(text) => text.clone(),
            value => value.to_string(),
        },
    }
}

fn _note_kind_desc(kind: u64) -> String {
    match kind {
        1 => "microblog".to_string(),
        0 => "profile".to_string(),
        _ => kind.to_string(),
    }
}

fn present_tool() -> Tool {
    Tool {
        parse_call: ToolParse::Typed(PresentNotesCall::parse),
        spec: ToolSpec::new(
            "present_notes",
            "A tool for presenting notes to the user for display. Should be called at the end of a response so that the UI can present the notes referred to in the previous message.",
            vec![ToolArg::new(
                "note_ids",
                ToolArgType::String,
                "A comma-separated list of hex note ids",
            )
            .required(true)],
        ),
    }
}

fn query_tool() -> Tool {
    Tool {
        parse_call: ToolParse::Typed(QueryCall::parse),
        spec: QueryCall::spec(),
    }
}

pub fn dave_tools() -> Vec<Tool> {
    vec![query_tool(), present_tool()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app tool's call keeps the model's arguments as JSON, takes none
    /// as an empty object, and refuses arguments that aren't an object.
    #[test]
    fn an_app_tools_call_keeps_its_arguments_as_json() {
        let spec = ToolSpec::new("headway_show_board", "Show a board.", vec![]);
        let tools = HashMap::from([(spec.name().to_owned(), Tool::app(spec))]);
        let call = |args: &str| {
            UnknownToolCall {
                id: "1".into(),
                name: "headway_show_board".into(),
                arguments: args.into(),
            }
            .parse(&tools)
        };

        let parsed = call(r#"{"board":"dave"}"#).unwrap();
        let ToolCalls::App(app) = parsed.calls() else {
            panic!("an app call");
        };
        assert_eq!(app.name, "headway_show_board");
        assert_eq!(app.args, json!({ "board": "dave" }));
        assert_eq!(parsed.calls().api_name(), "headway_show_board");

        let ToolCalls::App(empty) = call("").unwrap().calls().clone() else {
            panic!("an app call");
        };
        assert_eq!(empty.args, json!({}));

        assert!(matches!(
            call("[1]"),
            Err(ToolCallError::ArgParseFailure(_))
        ));
    }

    /// Regression: tool descriptions with defaults had a double closing paren
    /// e.g. "(Default: 50))" instead of "(Default: 50)".
    #[test]
    fn tool_description_default_no_double_paren() {
        for tool in dave_tools() {
            let params = tool.spec().json_schema();
            let props = params.get("properties").unwrap().as_object().unwrap();
            for (arg_name, arg_schema) in props {
                let desc = arg_schema
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                assert!(
                    !desc.contains("))"),
                    "Arg '{}' has double closing paren in description: {:?}",
                    arg_name,
                    desc
                );
            }
        }
    }
}
