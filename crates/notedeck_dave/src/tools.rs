use async_openai::types::*;
use chrono::DateTime;
use enostr::{NoteId, Pubkey};
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, fmt};

/// A tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    typ: ToolCalls,
}

impl ToolCall {
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

    pub fn to_api(&self) -> ChatCompletionMessageToolCall {
        ChatCompletionMessageToolCall {
            id: self.id.clone(),
            r#type: ChatCompletionToolType::Function,
            function: self.typ.to_api(),
        }
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
    notes: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Error(String),
    Query(QueryResponse),
    PresentNotes(i32),
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

        let parsed_args = (tool.parse_call)(&self.arguments)?;
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
}

impl ToolCalls {
    pub fn to_api(&self) -> FunctionCall {
        FunctionCall {
            name: self.name().to_owned(),
            arguments: self.arguments(),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Query(_) => "search",
            Self::Invalid(_) => "error",
            Self::PresentNotes(_) => "present",
        }
    }

    fn arguments(&self) -> String {
        match self {
            Self::Query(search) => serde_json::to_string(search).unwrap(),
            Self::Invalid(partial) => serde_json::to_string(partial).unwrap(),
            Self::PresentNotes(call) => serde_json::to_string(&call.to_simple()).unwrap(),
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
            ToolCallError::NotFound(name) => write!(f, "tool '{name}' not found"),
            ToolCallError::ArgParseFailure(msg) => {
                write!(f, "failed to parse arguments: {msg}")
            }
        }
    }
}

#[derive(Debug, Clone)]
enum ArgType {
    String,
    Number,

    #[allow(dead_code)]
    Enum(Vec<&'static str>),
}

impl ArgType {
    pub fn type_string(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Enum(_) => "string",
        }
    }
}

#[derive(Debug, Clone)]
struct ToolArg {
    typ: ArgType,
    name: &'static str,
    required: bool,
    description: &'static str,
    default: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct Tool {
    parse_call: fn(&str) -> Result<ToolCalls, ToolCallError>,
    name: &'static str,
    description: &'static str,
    arguments: Vec<ToolArg>,
}

impl Tool {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn to_function_object(&self) -> FunctionObject {
        let required_args = self
            .arguments
            .iter()
            .filter_map(|arg| {
                if arg.required {
                    Some(Value::String(arg.name.to_owned()))
                } else {
                    None
                }
            })
            .collect();

        let mut parameters: serde_json::Map<String, Value> = serde_json::Map::new();
        parameters.insert("type".to_string(), Value::String("object".to_string()));
        parameters.insert("required".to_string(), Value::Array(required_args));
        parameters.insert("additionalProperties".to_string(), Value::Bool(false));

        let mut properties: serde_json::Map<String, Value> = serde_json::Map::new();

        for arg in &self.arguments {
            let mut props: serde_json::Map<String, Value> = serde_json::Map::new();
            props.insert(
                "type".to_string(),
                Value::String(arg.typ.type_string().to_string()),
            );

            let description = if let Some(default) = &arg.default {
                format!("{} (Default: {default}))", arg.description)
            } else {
                arg.description.to_owned()
            };

            props.insert("description".to_string(), Value::String(description));
            if let ArgType::Enum(enums) = &arg.typ {
                props.insert(
                    "enum".to_string(),
                    Value::Array(
                        enums
                            .iter()
                            .map(|s| Value::String((*s).to_owned()))
                            .collect(),
                    ),
                );
            }

            properties.insert(arg.name.to_owned(), Value::Object(props));
        }

        parameters.insert("properties".to_string(), Value::Object(properties));

        FunctionObject {
            name: self.name.to_owned(),
            description: Some(self.description.to_owned()),
            strict: Some(false),
            parameters: Some(Value::Object(parameters)),
        }
    }

    pub fn to_api(&self) -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: self.to_function_object(),
        }
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

    pub fn responses(&self) -> &ToolResponses {
        &self.typ
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

/// The parsed nostrdb query that dave wants to use to satisfy a request
#[derive(Debug, Deserialize, Serialize, Clone)]
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

    fn limit(&self) -> u64 {
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
/// tool responses
#[derive(Debug, Serialize)]
struct SimpleNote {
    note_id: String,
    pubkey: String,
    name: String,
    content: String,
    created_at: String,
    note_kind: u64, // todo: add replying to
}

/// Take the result of a tool response and present it to the ai so that
/// it can interepret it and take further action
fn format_tool_response_for_ai(txn: &Transaction, ndb: &Ndb, resp: &ToolResponses) -> String {
    match resp {
        ToolResponses::PresentNotes(n) => format!("{n} notes presented to the user"),
        ToolResponses::Error(s) => format!("error: {}", &s),

        ToolResponses::Query(search_r) => {
            let simple_notes: Vec<SimpleNote> = search_r
                .notes
                .iter()
                .filter_map(|nkey| {
                    let Ok(note) = ndb.get_note_by_key(txn, NoteKey::new(*nkey)) else {
                        return None;
                    };

                    let name = ndb
                        .get_profile_by_pubkey(txn, note.pubkey())
                        .ok()
                        .and_then(|p| p.record().profile())
                        .and_then(|p| p.name().or_else(|| p.display_name()))
                        .unwrap_or("Anonymous")
                        .to_string();

                    let content = note.content().to_owned();
                    let pubkey = hex::encode(note.pubkey());
                    let note_kind = note.kind() as u64;
                    let note_id = hex::encode(note.id());

                    let created_at = {
                        let datetime =
                            DateTime::from_timestamp(note.created_at() as i64, 0).unwrap();
                        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                    };

                    Some(SimpleNote {
                        note_id,
                        pubkey,
                        name,
                        content,
                        created_at,
                        note_kind,
                    })
                })
                .collect();

            serde_json::to_string(&json!({"search_results": simple_notes})).unwrap()
        }
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
        name: "present_notes",
        parse_call: PresentNotesCall::parse,
        description: "A tool for presenting notes to the user for display. Should be called at the end of a response so that the UI can present the notes referred to in the previous message.",
        arguments: vec![ToolArg {
            name: "note_ids",
            description: "A comma-separated list of hex note ids",
            typ: ArgType::String,
            required: true,
            default: None,
        }],
    }
}

fn query_tool() -> Tool {
    Tool {
        name: "query",
        parse_call: QueryCall::parse,
        description: "Note query functionality. Used for finding notes using full-text search terms, scoped by different contexts. You can use a combination of limit, since, and until to pull notes from any time range.",
        arguments: vec![
            ToolArg {
                name: "search",
                typ: ArgType::String,
                required: false,
                default: None,
                description: "A fulltext search query. Queries with multiple words will only return results with notes that have all of those words. Don't include filler words/symbols like 'and', punctuation, etc",
            },

            ToolArg {
                name: "limit",
                typ: ArgType::Number,
                required: true,
                default: Some(Value::Number(serde_json::Number::from_i128(50).unwrap())),
                description: "The number of results to return.",
            },

            ToolArg {
                name: "since",
                typ: ArgType::Number,
                required: false,
                default: None,
                description: "Only pull notes after this unix timestamp",
            },

            ToolArg {
                name: "until",
                typ: ArgType::Number,
                required: false,
                default: None,
                description: "Only pull notes up until this unix timestamp. Always include this when searching notes within some date range (yesterday, last week, etc).",
            },

            ToolArg {
                name: "author",
                typ: ArgType::String,
                required: false,
                default: None,
                description: "An author *pubkey* to constrain the query on. Can be used to search for notes from individual users. If unsure what pubkey to u
se, you can query for kind 0 profiles with the search argument.",
            },

            ToolArg {
                name: "kind",
                typ: ArgType::Number,
                required: false,
                default: Some(Value::Number(serde_json::Number::from_i128(1).unwrap())),
                description: r#"The kind of note. Kind list:
                - 0: profiles
                - 1: microblogs/\"tweets\"/posts
                - 6: reposts of kind 1 notes
                - 7: emoji reactions/likes
                - 9735: zaps (bitcoin micropayment receipts)
                - 30023: longform articles, blog posts, etc

                "#,
            },

        ]
    }
}

pub fn dave_tools() -> Vec<Tool> {
    vec![query_tool(), present_tool()]
}
