# AI Conversation Nostr Notes — Design Spec

## Overview

Represent claude-code session JSONL lines as nostr events, enabling:
1. **Session presentation resume** — reload a previous session's UI from local nostr DB without re-parsing JSONL
2. **Round-trip fidelity** — reconstruct the original JSONL from nostr events for claude-code `--resume`
3. **Future sharing** — structure is ready for publishing sessions to relays (with privacy considerations deferred)

## Architecture

```
claude-code JSONL  ──→  nostr events  ──→  ndb.process_event (local)
                            │
                            ├──→  UI rendering (presentation from nostr query)
                            └──→  JSONL reconstruction (for --resume)
```

## Event Structure

**Kind**: Regular event (1000-9999 range, specific number TBD). Immutable — no replaceable events.

Each JSONL line becomes one nostr event. Every message type (user, assistant, tool_call, tool_result, progress, etc.) gets its own note for 1:1 JSONL line reconstruction.

### Note Format

```json
{
  "kind": "<TBD>",
  "content": "<human-readable presentation text>",
  "tags": [
    // Session identity
    ["d", "<session-id>"],
    ["session-slug", "<human-readable-name>"],

    // Threading (NIP-10)
    ["e", "<root-note-id>", "", "root"],
    ["e", "<parent-note-id>", "", "reply"],

    // Message metadata
    ["source", "claude-code"],
    ["source-version", "2.1.42"],
    ["role", "<user|assistant|system|tool_call|tool_result>"],
    ["model", "claude-opus-4-6"],
    ["turn-type", "<JSONL type field: user|assistant|progress|queue-operation|file-history-snapshot>"],

    // Discoverability
    ["t", "ai-conversation"],

    // Lossless reconstruction (Option 3)
    ["source-data", "<JSON-escaped JSONL line with paths normalized>"]
  ]
}
```

### Content Field

Human-readable text suitable for rendering in any nostr client:
- **user**: The user's message text
- **assistant**: The assistant's rendered markdown text (text blocks only)
- **tool_call**: Summary like `Glob: {"pattern": "**/*.rs"}` or tool name + input preview
- **tool_result**: The tool output text (possibly truncated for presentation)
- **progress**: Description of the progress event
- **queue-operation / file-history-snapshot**: Minimal description

### source-data Tag

Contains the **full JSONL line** as a JSON string with these transformations applied:
- **Path normalization**: All absolute paths converted to relative (using `cwd` as base)
- **Sensitive data stripping** (TODO — deferred to later task):
  - Token usage / cache statistics
  - API request IDs
  - Permission mode details

On reconstruction, relative paths are re-expanded using the local machine's working directory.

## JSONL Line Type → Nostr Event Mapping

| JSONL `type` | `role` tag | `content` | Notes |
|---|---|---|---|
| `user` (text) | `user` | User's message text | Simple text content |
| `user` (tool_result) | `tool_result` | Tool output text | Separated from user text |
| `assistant` (text) | `assistant` | Rendered markdown | Text blocks from content array |
| `assistant` (tool_use) | `tool_call` | Tool name + input summary | Each tool_use block = separate note |
| `progress` | `progress` | Hook progress description | Mapped for round-trip fidelity |
| `queue-operation` | `queue-operation` | Operation type | Mapped for round-trip fidelity |
| `file-history-snapshot` | `file-history-snapshot` | Snapshot summary | Mapped for round-trip fidelity |

**Important**: Assistant messages with mixed content (text + tool_use blocks) are split into multiple nostr events — one per content block. Each gets its own note, threaded in sequence via `e` tags.

## Conversation Threading

Uses **NIP-10** reply threading:
- First note in a session: no `e` tags (it is the root)
- All subsequent notes: `["e", "<root-id>", "", "root"]` + `["e", "<prev-id>", "", "reply"]`
- The `e` tags always reference **nostr note IDs** (not JSONL UUIDs)
- UUID-to-note-ID mapping is maintained during conversion

## Path Normalization

When converting JSONL → nostr events:
1. Extract `cwd` from the JSONL line
2. All absolute paths that start with `cwd` are converted to relative paths
3. `cwd` itself is stored as a relative path (or stripped, with project root as implicit base)

When reconstructing nostr events → JSONL:
1. Determine local working directory
2. Re-expand all relative paths to absolute using local `cwd`
3. Update `cwd`, `gitBranch`, and machine-specific fields

## Data Flow (Phase 1 — Local Only)

### Publishing (JSONL → nostr events)
1. On session activity, dave reads new JSONL lines
2. Each line is converted to a nostr event (normalize paths, extract presentation content)
3. Events are inserted via `ndb.process_event()` (local relay only)
4. UUID-to-note-ID mapping is cached for threading

### Consuming (nostr events → UI)
1. Query ndb for events with the session's `d` tag
2. Order by `e` tag threading (NIP-10 reply chain)
3. Render `content` field directly in the conversation UI
4. `role` tag determines message styling (user bubble, assistant bubble, tool collapse, etc.)

### Reconstruction (nostr events → JSONL, for future resume)
1. Query ndb for all events in a session (by `d` tag)
2. Order by reply chain
3. Extract `source-data` tag from each event
4. De-normalize paths (relative → absolute for local machine)
5. Write as JSONL file
6. Resume via `claude --resume <session-id>`

## Non-Goals (Phase 1)

- Publishing to external relays (privacy concerns)
- Resuming shared sessions from other users
- Sensitive data stripping (noted as TODO)
- NIP proposal (informal notedeck convention for now)
