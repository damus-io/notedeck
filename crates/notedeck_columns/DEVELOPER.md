# NoteDeck Columns - Developer Guide

This document provides detailed information for developers who want to contribute to or understand the NoteDeck Columns codebase.

## Project Structure

The NoteDeck Columns codebase is organized as follows:

```
notedeck_columns
├── src
│   ├── ui               # UI components and views
│   │   ├── note         # Note-related UI components (posts, replies, quotes)
│   │   ├── column       # Column UI components
│   │   ├── search       # Search functionality
│   │   ├── profile      # Profile views and editing
│   │   └── ...
│   ├── timeline         # Timeline data structures and logic
│   ├── storage          # Persistence mechanisms
│   ├── accounts         # Account management
│   ├── decks            # Deck management
│   ├── app.rs           # Main application logic
│   ├── app_creation.rs  # Application initialization
│   ├── route.rs         # Routing system
│   ├── nav.rs           # Navigation logic
│   └── ...
```

## Development Setup

### Prerequisites

- Rust toolchain (latest stable recommended)
- [nostrdb](https://github.com/damus-io/nostrdb) and its dependencies
- egui and eframe

### Building the Project

1. Clone the repository:
   ```bash
   git clone https://github.com/damus-io/notedeck
   cd notedeck
   ```

2. Build the project:
   ```bash
   cargo build --release
   ```

3. Run the application:
   ```bash
   cargo run --release
   ```

### Development Mode

For development, you might want to run with debug symbols:

```bash
cargo run
```

## Core Concepts

### Decks and Columns

- **Deck**: A collection of columns that a user can switch between
- **Column**: A view into a specific type of Nostr content (timeline, profile, etc.)

### Timelines

Timelines are a fundamental concept in NoteDeck Columns:

- `Timeline`: Represents a stream of notes with filters and subscriptions
- `TimelineKind`: Defines the type of timeline (Universe, Profile, Notifications, etc.)
- `TimelineTab`: Filtered views of a timeline (e.g., Notes only vs. Notes & Replies)
- `TimelineCache`: Caches timeline data for efficient access

### Navigation and Routing

- `Route`: Represents application navigation targets
- `Router`: Manages the navigation stack for each column
- `NavTitle`: Renders the title bar for navigation
- `RenderNavAction`: Actions resulting from navigation events

### UI Components

The UI is built with egui and organized into components:

- `PostView`, `PostReplyView`, `QuoteRepostView`: Note creation UI
- `NoteView`: Displays Nostr notes
- `ProfileView`: Displays and edits profiles
- `TimelineView`: Renders timelines in columns
- `DesktopSidePanel`: Side navigation panel

## Key Implementation Details

### Subscriptions and Realtime Updates

NoteDeck Columns manages Nostr subscriptions and relay connections to provide realtime updates:

- `MultiSubscriber`: Handles subscriptions to multiple relays
- `Subscriptions`: Tracks application-wide subscriptions
- `RelayPool`: Manages relay connections

### Data Flow

1. User actions create routes or trigger navigation
2. Routes are mapped to timeline kinds or other UI views
3. Timelines query nostrdb for notes matching their filters
4. UI components render the note data
5. Subscriptions keep the data updated in realtime

### State Management

State is managed at different levels:

- `Damus`: Contains global application state
- `DecksCache`: Holds deck and column configurations
- `TimelineCache`: Caches timeline data
- Various component-specific state structures

## Testing

Run the test suite:

```bash
cargo test
```

The codebase includes unit tests for critical components.

## Common Tasks

### Adding a New Column Type

1. Add a new variant to `TimelineKind` enum in `timeline/kind.rs`
2. Implement the necessary filter logic
3. Update the serialization and parsing methods
4. Add UI support in the AddColumn view

### Adding UI Components

1. Create a new Rust file in the appropriate ui directory
2. Implement the component using egui
3. Connect it to the routing system if needed

### Implementing New Features

When implementing new features:

1. Start by understanding the relevant parts of the codebase
2. Look for similar implementations as reference
3. Follow the existing patterns for state management and UI components
4. Add appropriate tests
5. Update documentation

## Troubleshooting

### Common Issues

- **Render Issues**: Check the egui-related code for layout problems
- **Data Freshness**: Verify subscription and filter setup
- **Performance**: Look for inefficient queries or rendering

### Debugging

- Use `tracing` macros (`debug!`, `info!`, `error!`) for logging
- Run with `RUST_LOG=debug` for verbose output
- Use `cargo expand` to inspect macro expansion

## Architecture Decisions

### Why egui?

egui was chosen for its immediate mode rendering approach and Rust integration, making it well-suited for a responsive multi-column UI.

### Why nostrdb?

nostrdb provides high-performance local storage and querying for Nostr events, which is essential for a responsive client.

### Timeline-centric Design

The codebase is structured around timelines because they provide a natural abstraction for the different types of Nostr content views needed in a column-based interface.

## Contributing

1. Fork the repository
2. Create a feature branch: `git checkout -b feature/my-feature`
3. Make your changes
4. Run tests: `cargo test`
5. Submit a pull request

Please follow the existing code style and patterns.
