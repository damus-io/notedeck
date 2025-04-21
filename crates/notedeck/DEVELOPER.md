# Notedeck Developer Documentation

This document provides technical details and guidance for developers working with the Notedeck crate.

## Architecture Overview

Notedeck is built around a modular architecture that separates concerns into distinct components:

### Core Components

1. **App Framework (`app.rs`)**
   - `Notedeck` - The main application framework that ties everything together
   - `App` - The trait that specific applications must implement

2. **Data Layer**
   - `Ndb` - NostrDB database for efficient storage and querying
   - `NoteCache` - In-memory cache for expensive-to-compute note data like nip10 structure
   - `Images` - Image and GIF cache management

3. **Network Layer**
   - `RelayPool` - Manages connections to Nostr relays
   - `UnknownIds` - Tracks and resolves unknown profiles and notes

4. **User Accounts**
   - `Accounts` - Manages user keypairs and account information
   - `AccountStorage` - Handles persistent storage of account data

5. **Wallet Integration**
   - `Wallet` - Lightning wallet integration
   - `Zaps` - Handles Nostr zap functionality

6. **UI Components**
   - `NotedeckTextStyle` - Text styling utilities
   - `ColorTheme` - Theme management
   - Various UI helpers

## Key Concepts

### Note Context and Actions

Notes have associated context and actions that define how users can interact with them:

```rust
pub enum NoteAction {
    Reply(NoteId),      // Reply to a note
    Quote(NoteId),      // Quote a note
    Hashtag(String),    // Click on a hashtag
    Profile(Pubkey),    // View a profile
    Note(NoteId),       // View a note
    Context(ContextSelection), // Context menu options
    Zap(ZapAction),     // Zap (tip) interaction
}
```

### Relay Management

Notedeck handles relays through the `RelaySpec` structure, which implements NIP-65 functionality for marking relays as read or write.

### Filtering and Subscriptions

The `FilterState` enum manages the state of subscriptions to Nostr relays:

```rust
pub enum FilterState {
    NeedsRemote(Vec<Filter>),
    FetchingRemote(UnifiedSubscription),
    GotRemote(Subscription),
    Ready(Vec<Filter>),
    Broken(FilterError),
}
```

## Development Workflow

### Setting Up Your Environment

1. Clone the repository
2. Build with `cargo build`
3. Test with `cargo test`

### Creating a New Notedeck App

1. Import the notedeck crate
2. Implement the `App` trait
3. Use the `Notedeck` struct as your application framework

Example:

```rust
use notedeck::{App, Notedeck, AppContext};

struct MyNostrApp {
    // Your app-specific state
}

impl App for MyNostrApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        // Your app's UI and logic here
    }
}

fn main() {
    let notedeck = Notedeck::new(...).app(MyNostrApp { /* ... */ });
    // Run your app
}
```

### Working with Notes

Notes are the core data structure in Nostr. Here's how to work with them:

```rust
// Get a note by ID
let txn = Transaction::new(&ndb).expect("txn");
if let Ok(note) = ndb.get_note_by_id(&txn, note_id.bytes()) {
    // Process the note
}

// Create a cached note
let cached_note = note_cache.cached_note_or_insert(note_key, &note);
```

### Adding Account Management

Account management is handled through the `Accounts` struct:

```rust
// Add a new account
let action = accounts.add_account(keypair);
action.process_action(&mut unknown_ids, &ndb, &txn);

// Get the current account
if let Some(account) = accounts.get_selected_account() {
    // Use the account
}
```

## Advanced Topics

### Zaps Implementation

Notedeck implements the zap (tipping) functionality according to the Nostr protocol:

1. Creates a zap request note (kind 9734)
2. Fetches a Lightning invoice via LNURL or LUD-16
3. Pays the invoice using a connected wallet
4. Tracks the zap state

### Image Caching

The image caching system efficiently manages images and animated GIFs:

1. Downloads images from URLs
2. Stores them in a local cache
3. Handles conversion between formats
4. Manages memory usage

### Persistent Storage

Notedeck provides several persistence mechanisms:

- `AccountStorage` - For user accounts
- `TimedSerializer` - For settings that need to be saved after a delay
- Various handlers for specific settings (zoom, theme, app size)

## Troubleshooting

### Common Issues

1. **Relay Connection Issues**
   - Check network connectivity
   - Verify relay URLs are correct
   - Look for relay debug messages

2. **Database Errors**
   - Ensure the database path is writable
   - Check for database corruption
   - Increase map size if needed

3. **Performance Issues**
   - Monitor the frame history
   - Check for large image caches
   - Consider reducing the number of active subscriptions

## Contributing

When contributing to Notedeck:

1. Follow the existing code style
2. Add tests for new functionality
3. Update documentation as needed
4. Keep performance in mind, especially for mobile targets
