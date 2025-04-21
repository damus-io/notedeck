# Developer Guide for Notedeck Chrome

This guide covers the technical details of the Notedeck Chrome component, which serves as the container and navigation framework for the Notedeck Nostr browser.

## Project Structure

```
notedeck_chrome
├── Cargo.toml          - Project manifest and dependencies
├── android/            - Android-specific code and configuration
│   └── ...
└── src/
    ├── notedeck.rs     - Main application entry point
    ├── lib.rs          - Library exports
    ├── theme.rs        - Theme definitions and customization
    ├── preview.rs      - UI component preview system
    ├── chrome.rs       - Core Chrome UI implementation
    ├── fonts.rs        - Font loading and configuration
    ├── app.rs          - Application management
    ├── android.rs      - Android-specific code
    └── setup.rs        - Application setup and configuration
```

## Key Components

### Chrome

The `Chrome` struct (`src/chrome.rs`) is the main container that:
- Maintains a list of applications
- Renders the sidebar
- Handles application switching
- Processes UI actions

### NotedeckApp

The `NotedeckApp` enum (`src/app.rs`) represents different applications that can be managed by the Chrome:
- `Columns` - The main Damus columns interface
- `Dave` - The Dave application
- `Other` - Generic container for other implementations of the `App` trait

### Setup

The `setup.rs` file handles initialization of:
- Font loading
- Theme setup
- Window configuration
- App icons

## Architecture Overview

Notedeck Chrome follows a container-based architecture:

1. The `Chrome` struct maintains a vector of applications
2. It controls which application is active via an index
3. The sidebar is rendered with buttons for each application
4. When an application is selected, it's updated within the container

## Android Support

Android integration relies on:
- Native Android UI integration via `GameActivity`
- Custom keyboard height detection for improved mobile UX
- Configuration via external JSON files

### Android Keyboard Handling

The Android integration includes custom Java code to handle keyboard visibility changes:
- `KeyboardHeightProvider` - Detects keyboard height changes
- `KeyboardHeightObserver` - Interface for keyboard events
- `MainActivity` - Main Android activity with JNI integration

## Styling and Theming

The theme system supports:
- Light and dark mode
- OLED-optimized dark mode for mobile
- Customizable text styles
- Font loading with multiple typefaces

## Building and Running

### Desktop

```bash
# Run in debug mode
cargo run -- --debug

# Run in release mode
cargo run --release
```

## Testing

The project includes tests for:
- Database path configuration
- Command-line argument parsing
- Column initialization

Run tests with:

```bash
cargo test
```

## Configuration and Data Paths

- Desktop: Uses the platform-specific data location or current directory
- Android: Uses the Android app's internal storage
- Custom paths can be specified via command-line arguments

## Advanced Debugging

- Enable the `debug-widget-callstack` feature to debug UI hierarchy
- Enable the `debug-interactive-widgets` feature to highlight interactive areas
- Android logging uses `tracing-logcat` for detailed diagnostics

## Code Workflow

1. `notedeck.rs` is the entry point, which initializes `Notedeck`
2. `setup.rs` configures the application environment
3. `Chrome` is created and populated with applications
4. The main UI loop renders the sidebar and active application

## Key Files for Modification

- `chrome.rs` - To modify the sidebar or app container behavior
- `theme.rs` - To update theming and colors
- `setup.rs` - To change startup configuration
- `android.rs` - For Android-specific changes

## Adding a New Application

1. Implement the `notedeck::App` trait for your application
2. Add a new variant to the `NotedeckApp` enum if needed
3. Update the `Chrome::topdown_sidebar` method to add a button for your app
4. Add your app to the `Chrome` instance in `notedeck.rs`
