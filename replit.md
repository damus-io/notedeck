# Notedeck - Nostr Desktop Client

## Overview

Notedeck is a modern, multiplatform Nostr client built with Rust and egui. It provides a TweetDeck-style multi-column interface for interacting with the Nostr protocol on desktop (Linux, macOS, Windows) and Android platforms.

**Version**: 0.7.1  
**License**: GPL  
**Status**: BETA

## Project Architecture

Notedeck follows a modular crate-based architecture:

### Core Crates

- **notedeck**: Core library with shared functionality, including:
  - NostrDB integration for efficient note storage and querying
  - Account management, contacts, and muting
  - Media handling (images, GIFs, blur effects)
  - Profile and note management
  - Zap (Lightning payments) support

- **notedeck_chrome**: UI container and navigation framework
  - Entry point for desktop and Android applications
  - Platform-specific implementations (Android integration)
  - Application lifecycle management

- **notedeck_columns**: TweetDeck-style column interface
  - Multi-column timeline views
  - Column management (add, remove, configure)
  - Timeline rendering and note display
  - Post composition and replies

- **notedeck_ui**: Shared UI components
  - Reusable widgets and components
  - Profile pictures, usernames, mentions
  - Note content rendering
  - Media viewers

### Application Crates

- **notedeck_dave**: AI assistant for Nostr
  - 3D avatar with WebGPU rendering
  - OpenAI/Ollama integration for natural language conversations
  - Tool-based architecture for querying and presenting Nostr content
  - NostrDB query interface

- **notedeck_calendar**: Calendar events application (NIP-52)
  - Date-based and time-based calendar events
  - Calendar management (kind 31924)
  - RSVP system (kind 31925)
  - Social features (comments, reposts, reactions)

- **notedeck_clndash**: Core Lightning dashboard
  - Channel management
  - Invoice tracking
  - Payment monitoring

- **notedeck_notebook**: Notebook/canvas application

### Supporting Crates

- **enostr**: Nostr protocol implementation
  - Client and relay message handling
  - Event filtering and subscriptions
  - Relay pool management

- **tokenator**: String token parsing library

## Technology Stack

- **Language**: Rust (1.90.0+)
- **UI Framework**: egui (0.31.1) with custom patches
- **Graphics**: WebGPU (via egui-wgpu)
- **Database**: NostrDB (embedded Nostr note database)
- **Async Runtime**: Tokio
- **Cryptography**: secp256k1 (Schnorr signatures)
- **Networking**: WebSocket (ewebsock)
- **AI Integration**: OpenAI API, Ollama support

## Nostr Protocol Implementation

Notedeck implements core Nostr NIPs (Nostr Implementation Possibilities):

- **NIP-01**: Basic protocol (events, signatures, relay communication)
- **NIP-52**: Calendar events (date-based, time-based, calendars, RSVPs)
- Additional NIPs for profiles, contacts, reactions, zaps, and more

### Event Types Supported

- **Regular Events** (kinds 1-999, 1000-9999): Stored by relays
- **Replaceable Events** (kinds 0, 3, 10000-19999): Only latest version kept
- **Ephemeral Events** (kinds 20000-29999): Not stored by relays
- **Addressable Events** (kinds 30000-39999): Replaceable by kind+pubkey+d-tag

## Development Setup

### Prerequisites

- Rust 1.90.0 or later
- Cargo (comes with Rust)

### Building

```bash
# Check if code compiles
cargo check --workspace

# Build all crates
cargo build --workspace

# Build in release mode
cargo build --release --workspace

# Run desktop app (requires display)
cargo run --release -p notedeck_chrome
```

### Android Development

```bash
# Install Android target
rustup target add aarch64-linux-android

# Build and install on connected device
cargo apk run --release -p notedeck_chrome
```

## Replit Environment

This project runs in the Replit cloud environment:

- **Workflow**: Build Check - Runs `cargo check --workspace` to verify compilation
- **Output**: Console-only (no GUI display available)
- **Purpose**: Code development, building, and verification

Since this is a desktop GUI application, it cannot be run with a display in the Replit environment. The workflow provides build verification to ensure code compiles correctly.

## Recent Changes

### 2025-10-13 (Late Evening)
- **Added interactive event viewing**: Click events to see detailed information
- Click events in week view or list view to view full details (title, time, location, participants, tags, description)
- Added event detail view with back button navigation
- **Context-aware navigation**: Arrows now navigate by day when in day view, by month in other views
- Day navigation properly handles month boundaries and refreshes events when crossing months

### 2025-10-13 (Evening)
- **Fixed calendar events not displaying**: Added relay message processing loop to ingest events into NostrDB
- Calendar now properly receives and stores events from Nostr relays
- Events automatically refresh when new ones arrive from subscriptions
- Implemented full event lifecycle: subscription → relay response → NostrDB storage → UI display

### 2025-10-13 (Afternoon)  
- Added user feedback system (success/error messages for event creation)
- Implemented Back button using AppAction::ToggleChrome for navigation
- Fixed relay subscription for calendar events (kinds 31922/31923)

### 2025-10-13 (Morning)
- Initial Replit setup
- Configured Rust stable toolchain
- Set up build verification workflow
- Added Calendar app (notedeck_calendar) for NIP-52 calendar events
- Implemented comprehensive event creation form with all NIP-52 tags

## User Preferences

- Focus on clean, modular architecture following Dave's pattern
- Use NostrDB for efficient data access
- Follow NIP specifications closely for protocol compliance
- Maintain egui UI patterns and responsive design

## Key Design Patterns

### App Development (following Dave's example)

1. **UI Layer**: Implement egui-based UI with responsive layouts
2. **State Management**: Separate UI state from application logic
3. **NostrDB Integration**: Use transactions for efficient querying
4. **Event Handling**: Process Nostr events asynchronously
5. **Tool Systems**: For AI-enabled apps, use structured tool definitions

### NostrDB Querying

```rust
// Example query pattern
let txn = Transaction::new(ndb).unwrap();
let filter = nostrdb::Filter::new()
    .kinds([1])  // Text notes
    .limit(50)
    .build();
let results = ndb.query(txn, &[filter], 50);
```

## Documentation

Detailed documentation available in individual crates:
- [Notedeck Core](./crates/notedeck/DEVELOPER.md)
- [Notedeck Chrome](./crates/notedeck_chrome/DEVELOPER.md)
- [Notedeck Columns](./crates/notedeck_columns/DEVELOPER.md)
- [Dave AI Assistant](./crates/notedeck_dave/docs/README.md)
- [UI Components](./crates/notedeck_ui/docs/components.md)

## Contributing

1. Fork the repository
2. Create a feature branch
3. Follow existing code patterns and architecture
4. Run `cargo check` and `cargo clippy` before committing
5. Submit a Pull Request

## Security

For security issues, see [SECURITY.md](./SECURITY.md)

## Authors

- William Casarin <jb55@jb55.com>
- kernelkind <kernelkind@gmail.com>
- And [contributors](https://github.com/damus-io/notedeck/graphs/contributors)
