# Dave Developer Guide

This guide explains the architecture and implementation details of Dave, the Nostr AI assistant for Notedeck. It's intended to help developers understand how Dave works and how to use it as a reference for building their own Notedeck applications.

## Architecture Overview

Dave follows a modular architecture with several key components:

```
notedeck_dave
├── UI Layer (ui/mod.rs, ui/dave.rs)
├── Avatar System (avatar.rs, quaternion.rs, vec3.rs)
├── Core Logic (lib.rs)
├── AI Communication (messages.rs)
├── Tools System (tools.rs)
└── Configuration (config.rs)
```

### Component Breakdown

#### 1. UI Layer (`ui/dave.rs`)

The UI layer handles rendering the chat interface and processing user inputs. Key features:

- Chat message rendering for different message types (user, assistant, tool calls)
- Input box with keyboard shortcuts
- Tool response visualization (note rendering, search results)

The UI is built with egui and uses a responsive layout that adapts to different screen sizes.

#### 2. 3D Avatar (`avatar.rs`)

Dave includes a 3D avatar rendered with WebGPU:

- Implements a 3D cube with proper lighting and rotation
- Interactive dragging for manual rotation
- Random "nudge" animations during AI responses
- Custom WebGPU shader implementation

#### 3. Core Logic (`lib.rs`)

The `Dave` struct in `lib.rs` ties everything together:

- Manages conversation state
- Handles user interactions
- Processes AI responses
- Executes tool calls
- Coordinates UI updates

#### 4. AI Communication (`messages.rs`)

Dave communicates with AI services (OpenAI or Ollama) through:

- Message formatting for API requests
- Streaming token processing
- Tool call handling
- Response parsing

#### 5. Tools System (`tools.rs`)

The tools system enables Dave to perform actions based on AI decisions:

- `query` - Search for notes in the NostrDB
- `present_notes` - Display specific notes to the user

Each tool has a structured definition with:
- Name and description
- Parameter specifications
- Parsing logic
- Execution code

## Key Workflows

### 1. User Message Flow

When a user sends a message:

1. UI captures the input and triggers a `Send` action
2. The message is added to the chat history
3. A request is sent to the AI service with the conversation context
4. The AI response is streamed back token by token
5. The UI updates in real-time as tokens arrive

### 2. Tool Call Flow

When the AI decides to use a tool:

1. The tool call is parsed and validated
2. The tool is executed (e.g., querying NostrDB)
3. Results are formatted and sent back to the AI
4. The AI receives the results and continues the conversation
5. The UI displays both the tool call and its results

### 3. Note Presentation

When presenting notes:

1. The AI identifies relevant notes and calls `present_notes`
2. Note IDs are parsed and validated
3. The UI renders the notes in a scrollable horizontal view
4. The AI references these notes in its response with `^1`, `^2`, etc.

## Implementation Patterns

### Streaming UI Updates

Dave uses Rust's `mpsc` channels to handle streaming updates:

```rust
let (tx, rx) = mpsc::channel();
self.incoming_tokens = Some(rx);

// In a separate thread:
tokio::spawn(async move {
    // Process streaming responses
    while let Some(token) = token_stream.next().await {
        // Send tokens back to the UI thread
        tx.send(DaveApiResponse::Token(content.to_owned()))?;
        ctx.request_repaint();
    }
});
```

### Tool Definition

Tools are defined with structured metadata:

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
            description: "A fulltext search query...",
            // ...
        },
        // Additional arguments...
    ]
}
```

### WebGPU Integration

The 3D avatar demonstrates WebGPU integration with egui:

```rust
ui.painter().add(egui_wgpu::Callback::new_paint_callback(
    rect,
    CubeCallback {
        mvp_matrix,
        model_matrix,
    },
));
```

## Using Dave as a Reference

### Building a Notedeck App

To build your own Notedeck app:

1. Implement the `notedeck::App` trait
2. Define your UI components and state management
3. Handle app-specific actions and updates

```rust
impl notedeck::App for YourApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        // Process events, update state
        // Render UI components
        // Handle user actions
    }
}
```

### Working with NostrDB

Dave demonstrates how to query and present Nostr content:

```rust
// Creating a transaction
let txn = Transaction::new(note_context.ndb).unwrap();

// Querying notes
let filter = nostrdb::Filter::new()
    .limit(limit)
    .search(search_term)
    .kinds([1])
    .build();
    
let results = ndb.query(txn, &[filter], limit as i32);

// Rendering notes
for note_id in &note_ids {
    let note = note_context.ndb.get_note_by_id(&txn, note_id.bytes());
    // Render the note...
}
```

### Implementing AI Tools

To add new tools:

1. Define a new call struct with parameters
2. Implement parsing logic
3. Add execution code
4. Register the tool in `dave_tools()`

```rust
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct YourToolCall {
    // Parameters
}

impl YourToolCall {
    fn parse(args: &str) -> Result<ToolCalls, ToolCallError> {
        // Parse JSON arguments
    }
    
    // Execution logic
}

// Add to tools list
pub fn dave_tools() -> Vec<Tool> {
    vec![query_tool(), present_tool(), your_tool()]
}
```

## Best Practices

1. **Responsive Design**: Use the `is_narrow()` function to adapt layouts for different screen sizes
2. **Streaming Updates**: Process large responses incrementally to keep the UI responsive
3. **Error Handling**: Gracefully handle API errors and unexpected inputs
4. **Tool Design**: Create tools with clear, focused functionality and descriptive metadata
5. **State Management**: Keep UI state separate from application logic

## Advanced Features

### Custom Rendering

Dave demonstrates custom rendering with WebGPU for the 3D avatar:

1. Define shaders using WGSL
2. Set up rendering pipelines and resources
3. Implement the `CallbackTrait` for custom drawing
4. Add paint callbacks to the UI

### AI Context Management

Dave maintains conversation context for the AI:

1. Structured message history (`Vec<Message>`)
2. Tool call results included in context
3. System prompt with instructions and constraints
4. Proper message formatting for API requests

## Conclusion

Dave is a sophisticated example of a Notedeck application that integrates AI, 3D rendering, and Nostr data. By studying its implementation, developers can learn patterns and techniques for building their own applications on the Notedeck platform.
