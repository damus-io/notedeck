# Dave's Tool System: In-Depth Guide

One of the most powerful aspects of Dave is its tools system, which allows the AI assistant to perform actions within the Notedeck environment. This guide explores the design and implementation of Dave's tools system, explaining how it enables the AI to query data and present content to users.

## Tools System Overview

The tools system enables Dave to:

1. Search the NostrDB for relevant notes
2. Present notes to users through the UI
3. Handle context-specific queries (home, profile, etc.)
4. Process streaming tool calls from the AI

## Core Components

### 1. Tool Definitions (`tools.rs`)

Each tool is defined with metadata that describes:
- Name and description
- Required and optional parameters
- Parameter types and constraints
- Parsing and execution logic

```rust
Tool {
    name: "query",
    parse_call: QueryCall::parse,
    description: "Note query functionality...",
    arguments: vec![
        ToolArg {
            name: "search",
            typ: ArgType::String,
            required: false,
            default: None,
            description: "A fulltext search query...",
        },
        // More arguments...
    ]
}
```

### 2. Tool Calls

When the AI decides to use a tool, it generates a tool call:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    id: String,
    typ: ToolCalls,
}
```

The `ToolCalls` enum represents different types of tool calls:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolCalls {
    Query(QueryCall),
    PresentNotes(PresentNotesCall),
}
```

### 3. Tool Responses

After executing a tool, Dave sends a response back to the AI:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResponse {
    id: String,
    typ: ToolResponses,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolResponses {
    Query(QueryResponse),
    PresentNotes,
}
```

### 4. Streaming Processing

Since tool calls arrive in a streaming fashion from the AI API, Dave uses a `PartialToolCall` structure to collect fragments:

```rust
#[derive(Default, Debug, Clone)]
pub struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}
```

## Available Tools

### 1. Query Tool

The query tool searches the NostrDB for notes matching specific criteria:

```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct QueryCall {
    context: Option<QueryContext>,
    limit: Option<u64>,
    since: Option<u64>,
    kind: Option<u64>,
    until: Option<u64>,
    search: Option<String>,
}
```

Parameters:
- `context`: Where to search (Home, Profile, Any)
- `limit`: Maximum number of results
- `since`/`until`: Time range constraints (unix timestamps)
- `kind`: Note type (1 for posts, 0 for profiles, etc.)
- `search`: Fulltext search query

Example usage by the AI:
```json
{
  "search": "bitcoin",
  "limit": 10,
  "context": "home",
  "kind": 1
}
```

### 2. Present Notes Tool

The present notes tool displays specific notes to the user:

```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PresentNotesCall {
    pub note_ids: Vec<NoteId>,
}
```

Parameters:
- `note_ids`: List of note IDs to display

Example usage by the AI:
```json
{
  "note_ids": "fe1278a57ce6a499cca6a54971f7255e5a953c91243f891be54c50155a7b9a9c,a8943f1c99af5acd5ebb24e7dae860ab8c879bdf2ed4bd14bbc28a3a4b0c2f50"
}
```

## Tool Execution Flow

1. **Tool Call Parsing**:
   - AI sends a tool call with ID, name, and arguments
   - Dave parses the JSON arguments into typed structures
   - Validation ensures required parameters are present

2. **Tool Execution**:
   - For query tool: Constructs a NostrDB filter and executes the query
   - For present notes: Validates note IDs and prepares them for display

3. **Response Formatting**:
   - Query results are formatted as JSON for the AI
   - Notes are prepared for UI rendering

4. **Response Processing**:
   - AI receives the tool response and incorporates it into the conversation
   - UI displays relevant components (search results, note previews)

## Technical Implementation

### Note Formatting for AI

When returning query results to the AI, Dave formats notes in a simplified JSON structure:

```rust
#[derive(Debug, Serialize)]
struct SimpleNote {
    note_id: String,
    pubkey: String,
    name: String,
    content: String,
    created_at: String,
    note_kind: u64,
}
```

## Using the Tools in Practice

### System Prompt Guidance

Dave's system prompt instructs the AI on how to use the tools effectively:

```
- You *MUST* call the present_notes tool with a list of comma-separated note id references when referring to notes so that the UI can display them. Do *NOT* include note id references in the text response, but you *SHOULD* use ^1, ^2, etc to reference note indices passed to present_notes.
- When a user asks for a digest instead of specific query terms, make sure to include both since and until to pull notes for the correct range.
- When tasked with open-ended queries such as looking for interesting notes or summarizing the day, make sure to add enough notes to the context (limit: 100-200) so that it returns enough data for summarization.
```

### UI Integration

The UI renders tool calls and responses:

```rust
fn tool_calls_ui(ctx: &mut AppContext, toolcalls: &[ToolCall], ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        for call in toolcalls {
            match call.calls() {
                ToolCalls::PresentNotes(call) => Self::present_notes_ui(ctx, call, ui),
                ToolCalls::Query(search_call) => {
                    ui.horizontal(|ui| {
                        egui::Frame::new()
                            .inner_margin(10.0)
                            .corner_radius(10.0)
                            .fill(ui.visuals().widgets.inactive.weak_bg_fill)
                            .show(ui, |ui| {
                                Self::search_call_ui(search_call, ui);
                            })
                    });
                }
            }
        }
    });
}
```

## Extending the Tools System

### Adding a New Tool

To add a new tool:

1. Define the tool call structure:
```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NewToolCall {
    // Parameters...
}
```

2. Add a new variant to `ToolCalls` enum:
```rust
pub enum ToolCalls {
    Query(QueryCall),
    PresentNotes(PresentNotesCall),
    NewTool(NewToolCall),
}
```

3. Implement parsing logic:
```rust
impl NewToolCall {
    fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        // Parse JSON arguments...
        Ok(ToolCalls::NewTool(parsed))
    }
}
```

4. Create tool definition and add to `dave_tools()`:
```rust
fn new_tool() -> Tool {
    Tool {
        name: "new_tool",
        parse_call: NewToolCall::parse,
        description: "Description...",
        arguments: vec![
            // Arguments...
        ]
    }
}

pub fn dave_tools() -> Vec<Tool> {
    vec![query_tool(), present_tool(), new_tool()]
}
```

### Handling Tool Responses

Add a new variant to `ToolResponses`:
```rust
pub enum ToolResponses {
    Query(QueryResponse),
    PresentNotes,
    NewTool(NewToolResponse),
}
```

Implement response formatting:
```rust
fn format_tool_response_for_ai(txn: &Transaction, ndb: &Ndb, resp: &ToolResponses) -> String {
    match resp {
        // Existing cases...
        ToolResponses::NewTool(response) => {
            // Format response as JSON...
        }
    }
}
```

## Advanced Usage Patterns

### Context-Aware Queries

The `QueryContext` enum allows the AI to scope searches:

```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum QueryContext {
    Home,
    Profile,
    Any,
}
```

### Time-Based Queries

Dave is configured with current and recent timestamps in the system prompt, enabling time-aware queries:

```
- The current date is {date} ({timestamp} unix timestamp if needed for queries).
- Yesterday (-24hrs) was {yesterday_timestamp}. You can use this in combination with `since` queries for pulling notes for summarizing notes the user might have missed while they were away.
```

### Filtering Non-Relevant Content

Dave filters out reply notes when performing queries to improve results:

```rust
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

// Used in filter creation
.custom(|n| !is_reply(n))
```

## Conclusion

The tools system is what makes Dave truly powerful, enabling it to interact with NostrDB and present content to users. By understanding this system, developers can:

1. Extend Dave with new capabilities
2. Apply similar patterns in other AI-powered applications
3. Create tools that balance flexibility and structure
4. Build effective interfaces between AI models and application data

This architecture demonstrates a robust approach to enabling AI assistants to take meaningful actions within applications, going beyond simple text generation to deliver real utility to users.
