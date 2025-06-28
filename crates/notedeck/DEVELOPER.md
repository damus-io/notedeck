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

7. **Localization System**
   - `LocalizationManager` - Core localization functionality
   - `LocalizationContext` - Thread-safe context for sharing localization
   - Fluent-based translation system

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

### Localization System

Notedeck includes a comprehensive internationalization system built on the [Fluent](https://projectfluent.org/) translation framework. The system is designed for performance and developer experience.

#### Architecture

The localization system consists of several key components:

1. **LocalizationManager** - Core functionality for managing locales and translations
2. **LocalizationContext** - Thread-safe context for sharing localization across the application
3. **Fluent Resources** - Translation files in `.ftl` format stored in `assets/translations/`

#### Key Features

- **Efficient Caching**: Parsed Fluent resources and formatted strings are cached for performance
- **Thread Safety**: Uses `RwLock` for safe concurrent access
- **Dynamic Locale Switching**: Change languages at runtime without restarting
- **Argument Support**: Localized strings can include dynamic arguments
- **Development Tools**: Pseudolocale support for testing UI layout

#### Using the tr! and tr_plural! Macros

The `tr!` and `tr_plural!` macros are the primary way to use localization in Notedeck code. They provide a convenient, type-safe interface for getting localized strings.

##### The tr! Macro

```rust
use notedeck::tr;

// Simple string with comment
let welcome = tr!("Welcome to Notedeck!", "Main welcome message");
let cancel = tr!("Cancel", "Button label to cancel an action");

// String with parameters
let greeting = tr!("Hello, {name}!", "Greeting message", name="Alice");

// Multiple parameters
let message = tr!(
    "Welcome {name} to {app}!",
    "Welcome message with app name",
    name="Alice",
    app="Notedeck"
);

// In UI components
ui.button(tr!("Reply to {user}", "Reply button text", user="alice@example.com"));
```

##### The tr_plural! Macro

Use tr_plural! when there can be multiple variations of the same string depending on
some numeric count.

Not all languages follow the same pluralization rules

```rust
use notedeck::tr_plural;

// Simple pluralization
let count = 5;
let message = tr_plural!(
    "You have {count} note",     // Singular form
    "You have {count} notes",    // Plural form
    "Note count message",        // Comment
    count                        // Count value
);

// With additional parameters
let user = "Alice";
let message = tr_plural!(
    "{user} has {count} note",              // Singular
    "{user} has {count} notes",             // Plural
    "User note count message",              // Comment
    count,                                  // Count
    user=user                               // Additional parameter
);
```

##### Key Features

- **Automatic Key Normalization**: Converts messages and comments into valid FTL keys
- **Fallback Handling**: Falls back to original message if translation not found
- **Parameter Interpolation**: Automatically handles named parameters
- **Comment Context**: Provides context for translators

##### Best Practices

1. **Always Include Comments**: Comments provide context for translators
   ```rust
   // Good
   tr!("Add", "Button label to add something")

   // Bad
   tr!("Add", "")
   ```

2. **Use Descriptive Comments**: Make comments specific and helpful
   ```rust
   // Good
   tr!("Reply", "Button to reply to a note")

   // Bad
   tr!("Reply", "Reply")
   ```

3. **Consistent Parameter Names**: Use consistent parameter names across related strings
   ```rust
   // Consistent
   tr!("Follow {user}", "Follow button", user="alice")
   tr!("Unfollow {user}", "Unfollow button", user="alice")
   ```

4. **Always use tr_plural! for plural strings**: Not all languages follow English pluralization rules
   ```rust
   // Good
   // Each language can have more (or less) than just two pluralization forms.
   // Let the translators and the localization system help you figure that out implicitly.
   let message = tr_plural!(
      "You have {count} note",     // Singular form
      "You have {count} notes",    // Plural form
      "Note count message",        // Comment
      count                        // Count value
   );

   // Bad
   // Not all languages follow pluralization rules of English.
   // Some languages can have more (or less) than two variations!
   if count == 1 {
      tr!("You have 1 note", "Note count message")
   } else {
      tr!("You have {count} notes", "Note count message")
   }
   ```

#### Translation File Format

Translation files use the [Fluent](https://projectfluent.org/) format (`.ftl`).

Developers should never create their own `.ftl` files. Whenever user-facing strings are changed in code, run `python3 scripts/export_source_strings.py`. This script will generate `assets/translations/en-US/main.ftl` and `assets/translations/en-XA/main.ftl`. The format of the files look like the following:

```ftl
# Simple string
welcome_message = Welcome to Notedeck!

# String with arguments
welcome_user = Welcome {$name}!

# String with pluralization
note_count = {$count ->
    [1] One note
    *[other] {$count} notes
}
```

#### Adding New Languages

TODO

#### Development with Pseudolocale (en-XA)

For testing that all user-facing strings are going through the localization system and that the
UI layout renders well with different language translations, enable the pseudolocale:

```bash
NOTEDECK_PSEUDOLOCALE=1 cargo run -- --debug
```

The pseudolocale (`en-XA`) transforms English text in a way that is still readable but makes adjustments obvious enough that they are different from the original text (such as replacing English letters with accented equivalents), helping identify potential UI layout issues once it gets translated
to other languages.

Example transformations:
- "Add relay" → "[Àdd rélày]"
- "Cancel" → "[Çàñçél]"
- "Confirm" → "[Çóñfírm]"

#### Performance Considerations

- **Resource Caching**: Parsed Fluent resources are cached per locale
- **String Caching**: Simple strings (without arguments) are cached for repeated access
- **Cache Management**: Caches are automatically cleared when switching locales
- **Memory Limits**: String cache size can be limited to prevent memory growth

#### Testing Localization

The localization system includes comprehensive tests:

```bash
# Run localization tests
cargo test i18n
```

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

4. **Localization Issues**
   - Verify translation files exist in the correct directory structure
   - Check that locale codes are valid (e.g., `en-US`, `es-ES`)
   - Ensure FTL files are properly formatted
   - Look for missing translation keys in logs

## Contributing

When contributing to Notedeck:

1. Follow the existing code style
2. Add tests for new functionality
3. Update documentation as needed
4. Keep performance in mind, especially for mobile targets
5. For UI changes, test with pseudolocale enabled
6. When adding new strings, ensure they are properly localized
