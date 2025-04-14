use async_openai::types::*;
use chrono::DateTime;
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

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
#[derive(Default, Debug, Clone)]
pub struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
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
    context: QueryContext,
    notes: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Query(QueryResponse),
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

/// An enumeration of the possible tool calls that
/// can be parsed from Dave responses. When adding
/// new tools, this needs to be updated so that we can
/// handle tool call responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolCalls {
    Query(QueryCall),
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
        }
    }

    fn arguments(&self) -> String {
        match self {
            Self::Query(search) => serde_json::to_string(search).unwrap(),
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

#[derive(Debug, Clone)]
enum ArgType {
    String,
    Number,
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

    pub fn responses(&self) -> &ToolResponses {
        &self.typ
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum QueryContext {
    Home,
    Profile,
    Any,
}

/// The parsed nostrdb query that dave wants to use to satisfy a request
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct QueryCall {
    context: Option<QueryContext>,
    limit: Option<u64>,
    since: Option<u64>,
    kind: Option<u64>,
    until: Option<u64>,
    author: Option<String>,
    search: Option<String>,
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

    pub fn search(&self) -> Option<&str> {
        self.search.as_deref()
    }

    pub fn context(&self) -> QueryContext {
        self.context.clone().unwrap_or(QueryContext::Any)
    }

    pub fn execute(&self, txn: &Transaction, ndb: &Ndb) -> QueryResponse {
        let notes = {
            if let Ok(results) = ndb.query(txn, &[self.to_filter()], self.limit() as i32) {
                results.into_iter().map(|r| r.note_key.as_u64()).collect()
            } else {
                vec![]
            }
        };
        QueryResponse {
            context: self.context.clone().unwrap_or(QueryContext::Any),
            notes,
        }
    }

    pub fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        match serde_json::from_str::<QueryCall>(args) {
            Ok(call) => Ok(ToolCalls::Query(call)),
            Err(e) => Err(ToolCallError::ArgParseFailure(format!(
                "Failed to parse args: '{}', error: {}",
                args, e
            ))),
        }
    }
}

/// A simple note format for use when formatting
/// tool responses
#[derive(Debug, Serialize)]
struct SimpleNote {
    pubkey: String,
    name: String,
    content: String,
    created_at: String,
    note_kind: String, // todo: add replying to
}

/// Take the result of a tool response and present it to the ai so that
/// it can interepret it and take further action
fn format_tool_response_for_ai(txn: &Transaction, ndb: &Ndb, resp: &ToolResponses) -> String {
    match resp {
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
                    let note_kind = note_kind_desc(note.kind() as u64);

                    let created_at = {
                        let datetime =
                            DateTime::from_timestamp(note.created_at() as i64, 0).unwrap();
                        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
                    };

                    Some(SimpleNote {
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

fn note_kind_desc(kind: u64) -> String {
    match kind {
        1 => "microblog".to_string(),
        0 => "profile".to_string(),
        _ => kind.to_string(),
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

            ToolArg {
                name: "author",
                typ: ArgType::String,
                required: false,
                default: None,
                description: "An author *pubkey* to constrain the query on. Can be used to search for notes from individual users. If unsure what pubkey to use, you can query for kind 0 profiles with the search argument.",
            },

            ToolArg {
                name: "context",
                typ: ArgType::Enum(vec!["home", "profile", "any"]),
                required: false,
                default: Some(Value::String("any".to_string())),
                description: "The context in which the search is occuring. valid options are 'home', 'profile', 'any'",
            }
        ]
    }
}

pub fn dave_tools() -> Vec<Tool> {
    vec![query_tool()]
}
