# Dave - The Nostr AI Assistant

Dave is an AI-powered assistant for the Nostr protocol, built as a Notedeck application. It provides both a simple conversational interface for querying Nostr data and a full-featured agentic coding environment with Claude Code integration.

<img src="https://cdn.jb55.com/s/73ebab8f43804da8.png" width="50%"/>

## Overview

Dave serves two purposes:

1. **Nostr Assistant** - A conversational interface that can search, analyze, and present Nostr notes using natural language
2. **Agentic IDE** - A multi-agent development environment with Claude Code integration for coding tasks

You can use either mode depending on your needs—simple chat for quick Nostr queries, or agentic mode for software development workflows.

## Features

### Core Features

- [x] Interactive 3D avatar with WebGPU rendering
- [x] Natural language conversations with AI
- [x] Query and search the local Nostr database for notes
- [x] Present and render notes to the user
- [x] Tool-based architecture for AI actions
- [x] Multiple AI providers (OpenAI, Anthropic, Ollama)
- [ ] Context-aware searching (home, profile, or global scope)
- [ ] Anonymous [lmzap](https://jb55.com/lmzap) backend

### AI Modes

- **Chat Mode** - Simple OpenAI-style conversational interface for Nostr queries
- **Agentic Mode** - Full IDE with permissions, sessions, scene view, and Claude Code integration

### Multi-Agent System (Agentic Mode)

- **RTS-Style Scene View** - Visual grid-based view for managing multiple agents simultaneously
- **Focus Queue** - Priority-based system for agent attention (NeedsInput > Error > Done)
- **Auto-Steal Focus** - Automatically cycle through agents requiring attention
- **Session Management** - Multiple independent AI sessions with per-session working directories
- **Subagent Support** - Track and display Task tool subagents within chat history

### Claude Code Integration (Agentic Mode)

- **Interactive Permissions** - Allow/Deny tool calls with diff view for file changes
- **Auto-Accept Rules** - Configurable rules for automatically accepting safe operations:
  - Small edits (2 lines or less by default)
  - Read-only bash commands (grep, ls, cat, find, etc.)
  - Cargo commands (build, check, test, fmt, clippy)
- **Plan Mode** - Toggle plan mode with `Ctrl+M` for architectural planning
- **Session Resume** - Resume previous Claude Code sessions from filesystem
- **AskUserQuestion Support** - Answer multi-choice questions from the AI

### Keyboard-Driven Workflow (Agentic Mode)

| Shortcut | Action |
|----------|--------|
| `1` / `2` | Accept / Deny permission requests |
| `Shift+1` / `Shift+2` | Accept / Deny with custom message |
| `Escape` | Interrupt AI (double-press to confirm) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Cycle through agents |
| `Ctrl+1-9` | Jump to agent by number |
| `Ctrl+T` | New agent |
| `Ctrl+Shift+T` | Clone agent (same working directory) |
| `Ctrl+N` / `Ctrl+P` | Focus queue navigation (higher/lower priority) |
| `Ctrl+D` | Toggle Done status in focus queue |
| `Ctrl+M` | Toggle plan mode |
| `Ctrl+\` | Toggle auto-steal focus |
| `Ctrl+G` | Open external editor for input |
| `Ctrl+V` | Toggle scene view |
| `Delete` | Delete selected agent |

### UI Components

- **Diff View** - Syntax-highlighted diff for Edit/Write tool permission requests
- **Status Badges** - Visual indicators for plan mode, agent status, and keybinds
- **Keybind Hints** - Contextual hints shown when Ctrl is held
- **Directory Picker** - Select working directory when creating sessions
- **Session Picker** - Resume existing Claude Code sessions
- **Compaction Status** - Visual indicator when `/compact` is running

### External Integration

- **IPC Spawn** - Create agents from external tools via Unix domain socket
- **`notedeck-spawn` CLI** - Spawn agents from terminal: `notedeck-spawn /path/to/project`

## Technical Details

Dave uses:

- Egui for UI rendering
- WebGPU for 3D avatar visualization (optional)
- Claude Agent SDK for agentic workflows
- OpenAI API / Anthropic API / Ollama for chat mode
- NostrDB for Nostr note storage and querying
- Tokio for async operations
- Unix domain sockets for IPC

## Architecture

Dave is structured around several key components:

1. **UI Layer** - Handles rendering with scene view and chat panels
2. **Avatar** - 3D representation with WebGPU rendering
3. **Session Manager** - Manages multiple independent AI sessions
4. **Focus Queue** - Priority-based attention system for multi-agent workflows
5. **AI Backend** - Pluggable backends (Claude, OpenAI, Ollama)
6. **Tools System** - Provides structured ways for the AI to interact with Nostr data
7. **Auto-Accept Rules** - Configurable permission auto-approval
8. **IPC Listener** - External spawn requests via Unix socket
9. **Session Discovery** - Finds resumable Claude sessions from `~/.claude/projects/`

## Getting Started

### Chat Mode (OpenAI/Ollama)

1. Clone the repository
2. Set up your API keys:
   ```
   export OPENAI_API_KEY=your_api_key_here
   # or for Ollama
   export OLLAMA_HOST=http://localhost:11434
   ```
3. Build and run Notedeck with Dave

### Agentic Mode (Claude Code)

1. Install Claude Code CLI: `npm install -g @anthropic-ai/claude-code`
2. Authenticate: `claude login`
3. Run Dave and select a working directory when creating a new agent

## Configuration

Dave can be configured to use different AI backends:

- **OpenAI API** (Chat mode) - Set the `OPENAI_API_KEY` environment variable
- **Anthropic Claude** (Chat mode) - Set the `ANTHROPIC_API_KEY` environment variable
- **Ollama** (Chat mode) - Use a compatible model and set the `OLLAMA_HOST` environment variable
- **Claude Code** (Agentic mode) - Requires Claude Code CLI installed and authenticated

## File Locations

- IPC Socket:
  - Linux: `$XDG_RUNTIME_DIR/notedeck/spawn.sock`
  - macOS: `~/Library/Application Support/notedeck/spawn.sock`
- Claude Sessions: `~/.claude/projects/<project-path>/`

## Usage as a Reference

Dave serves as an excellent reference for developers looking to:

- Build conversational interfaces in Notedeck
- Implement 3D rendering with WebGPU in Rust applications
- Create tool-based AI agents that can take actions in response to user requests
- Query and present Nostr content in custom applications
- Build multi-agent systems with priority-based focus management
- Integrate Claude Code into custom applications

## Contributing

Contributions are welcome! See the issues list for planned features and improvements.

## License

GPL

## Related Projects

- [nostrdb](https://github.com/damus-io/nostrdb) - Embedded database for Nostr notes
- [Claude Code](https://claude.ai/claude-code) - Anthropic's agentic coding tool
- [Claude Agent SDK](https://github.com/anthropics/claude-code) - SDK for Claude Code integration
