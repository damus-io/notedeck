use crate::messages::ExecutedTool;
use async_openai::types::*;
use chrono::DateTime;
use enostr::NoteId;
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use notedeck::{ToolArg, ToolArgType, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashMap, fmt};

/// The nostrdb query tool call, shared with the browser-level tool module so
/// dave doesn't carry a second copy of the filter/execution logic.
pub use notedeck::QueryCall;

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
    pub notes: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Error(String),
    Query(QueryResponse),
    PresentNotes(i32),
    ExecutedTool(ExecutedTool),
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

    /// Returns the tool name as defined in the tool registry (for prompt-based tool calls)
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Query(_) => "query",
            Self::Invalid(_) => "invalid",
            Self::PresentNotes(_) => "present_notes",
        }
    }

    pub fn arguments(&self) -> String {
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
            ToolCallError::NotFound(ref name) => write!(f, "tool '{name}' not found"),
            ToolCallError::ArgParseFailure(ref msg) => {
                write!(f, "failed to parse arguments: {msg}")
            }
        }
    }
}

/// A dave tool: the portable [`ToolSpec`] advertised to the model (shared with
/// notedeck's browser-level tool module) plus dave's parser that turns the
/// model's raw arguments into a typed [`ToolCalls`] for dave's streaming,
/// persistence, and UI layers.
#[derive(Debug, Clone)]
pub struct Tool {
    parse_call: fn(&str) -> Result<ToolCalls, ToolCallError>,
    spec: ToolSpec,
}

impl Tool {
    pub fn name(&self) -> &'static str {
        self.spec.name()
    }

    pub fn description(&self) -> &'static str {
        self.spec.description()
    }

    pub fn parse_call(&self) -> fn(&str) -> Result<ToolCalls, ToolCallError> {
        self.parse_call
    }

    pub fn to_function_object(&self) -> FunctionObject {
        FunctionObject {
            name: self.spec.name().to_owned(),
            description: Some(self.spec.description().to_owned()),
            strict: Some(false),
            parameters: Some(self.spec.json_schema()),
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

    pub fn executed_tool(result: ExecutedTool) -> Self {
        Self {
            id: String::new(),
            typ: ToolResponses::ExecutedTool(result),
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

/// Parse a `query` tool call's raw arguments into a typed [`QueryCall`] wrapped
/// in dave's [`ToolCalls`]. The `QueryCall` type and its filter logic live in
/// notedeck's browser tool module; this is the dave-side adapter to its
/// streaming/parse machinery.
fn parse_query(args: &str) -> Result<ToolCalls, ToolCallError> {
    match serde_json::from_str::<QueryCall>(args) {
        Ok(call) => Ok(ToolCalls::Query(call)),
        Err(e) => Err(ToolCallError::ArgParseFailure(format!(
            "{args}, error: {e}"
        ))),
    }
}

/// Run a [`QueryCall`] and collect the matched note keys, using notedeck's
/// shared filter. Note keys are stable for the life of the db, so they are
/// resolved back to notes later when formatting the response for the AI.
pub fn execute_query(call: &QueryCall, txn: &Transaction, ndb: &Ndb) -> QueryResponse {
    let notes = ndb
        .query(txn, &[call.to_filter()], call.limit() as i32)
        .map(|results| results.into_iter().map(|r| r.note_key.as_u64()).collect())
        .unwrap_or_default();
    QueryResponse { notes }
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
        ToolResponses::Error(s) => format!("error: {}", &s),

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
        parse_call: PresentNotesCall::parse,
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
        parse_call: parse_query,
        spec: QueryCall::spec(),
    }
}

pub fn dave_tools() -> Vec<Tool> {
    vec![query_tool(), present_tool()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: tool descriptions with defaults had a double closing paren
    /// e.g. "(Default: 50))" instead of "(Default: 50)".
    #[test]
    fn tool_description_default_no_double_paren() {
        for tool in dave_tools() {
            let func_obj = tool.to_function_object();
            let params = func_obj.parameters.unwrap();
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
