# NoteDeck Columns

A TweetDeck-style multi-column interface for Nostr built with Rust and egui.

## Overview

NoteDeck Columns is a specialized UI component of the NoteDeck Nostr client that provides a TweetDeck-inspired multi-column layout for browsing Nostr content. It allows users to create customizable "decks" with multiple columns, each showing different types of Nostr content (home timeline, notifications, hashtags, profiles, etc.).

## Features

- **Multi-column layout**: View different Nostr content types side by side
- **Customizable decks**: Create and customize multiple decks for different use cases
- **Column types**:
  - Universe (global feed)
  - Contact lists (follows)
  - Profiles
  - Notifications
  - Hashtags
  - Threads
  - Search results
  - Algorithmic feeds (e.g., last notes per pubkey)
- **Interactions**: Post, reply, quote, and zap notes
- **Media support**: View and upload images
- **Multiple accounts**: Switch between multiple Nostr accounts

## Getting Started

NoteDeck Columns is part of the larger NoteDeck ecosystem. To use it:

1. Clone the NoteDeck repository
2. Build the project with Cargo
3. Run NoteDeck and select the Columns interface

See the [DEVELOPER.md](DEVELOPER.md) file for detailed setup instructions.

## Architecture

NoteDeck Columns is built using:

- **Rust**: For performance and type safety
- **egui**: For the UI rendering
- **nostrdb**: For Nostr data storage and retrieval
- **enostr**: For Nostr protocol communication

The codebase is organized around the concept of timelines, views, and decks, with a column-based UI architecture.

## Contributing

Contributions are welcome! Please see [DEVELOPER.md](DEVELOPER.md) for information on how to set up your development environment and contribute to the project.

## License

NoteDeck Columns is licensed under the [GPL v3](LICENSE).
