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
- Black (OLED-friendly) dark mode on every platform
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

## Keymap & macros

Bare keys (no modifier, nothing focused) belong to the chrome, not the apps.
Each frame, before the active app renders, `Chrome::dispatch_keys`
(`src/chrome/key_dispatch.rs`) matches the key press against the keymap for
the app's **key scopes** and runs the bound **UI tool** or macro. The keymap
is `src/keymap/default_keymap.toml`, with the account's overrides merged over
it. Keys typed into a text field never reach it: the app keeps those.

### The TOML format

```toml
[[bind]]
scope = "headway.grid"     # a key scope an app reports, or "global"
keys  = "g g"              # space-separated; Shift = uppercase;
                           # <Enter> <Esc> <Space> <C-f> <S-Space>
tool  = "headway_cursor_first"
label = "first card"       # the which-key strip's label (`?` shows it)
group = "move"             # adjacent bindings of a group and label share a hint
args  = { card = "{current}" }   # optional, passed to the tool as JSON;
                                 # "{current}" is the app's App::current_ref

[[bind]]
scope = "dave.normal"
keys  = "E"
macro = "session-explainer"      # in place of `tool`

[macro.session-explainer]        # tools run in turn, each once the last
label = "card explainer"         # has settled: navigation landed and the
                                 # next tool's app in front with it available
steps = [ { tool = "dave_open_issue" }, { tool = "headway_open_explainer" } ]
```

A macro step's `args` can name `"{current}"` too. It is replaced with the
current ref of the app in front **when the step runs**, so a step after a
navigation acts on where it landed. The step waits, like any other, until
that app has one:

```toml
[macro.session-done]             # open the session's card, then mark it done
steps = [ { tool = "dave_open_issue" },
          { tool = "headway_move_card", args = { card = "{current}", column = "done" } } ]
```

A step can also hand its **result** on, with no navigation in between. Give
it `as = "<name>"` and later steps read it as `{<name>}`; the step right
after reads it as `{prev}` too. Add `.field` (or `.0` for an array index) to
reach inside it. A string that is exactly one placeholder takes the value
itself, so numbers and objects keep their type; a placeholder inside longer
text is written into it.

```toml
[macro.session-done]             # mark the session's card done, staying in Dave
steps = [
  { tool = "dave_session_issue", as = "card" },        # {"issue": "headway:…"}
  { tool = "headway_move_card", args = { card = "{card.issue}", column = "done" } },
]
```

A step whose result is read must answer in the call: a data tool, or a UI
tool that returns data at once, such as `dave_session_issue`. Headway's UI
tools queue their work for its next frame and answer `null`, and an `as` step
that answers `null` aborts the run. A placeholder naming nothing defined
before it, `{prev}` in the first step, or an `as` of `current` or `prev`
fails the parse, so the keymap editor refuses the save.

App scopes are searched most specific first, then `global`, so an app's
binding of the same keys wins. A binding fires only while its tool is
available, so Esc with nothing to clear falls through to the next binding.
`src/keymap/parse.rs` has the full rules.

A user keymap is a delta, in the same format plus `unbind = true`: a user
`[[bind]]` of a scope and keys replaces every built-in binding of them,
`unbind = true` drops them, and a `[macro.<name>]` adds or replaces a macro by
name. A user binding or macro step naming a tool no app has fails the whole
merge, so a broken keymap never half-applies.

### Adding UI tools and key scopes to an app

1. Advertise the tools with `App::ui_tools` (static `ToolSpec`s marked
   `.ui()`) and run them in `App::call_ui_tool`, against the app's own
   `&mut self`.
2. Say which ones would do something right now in `App::ui_tool_available`.
   A key bound to an unavailable tool falls through.
3. Report the view's scopes from `App::key_scopes` as a
   `&'static [&'static str]`, so a frame with no key press allocates nothing.
   Report the thing the view is on from `App::current_ref` if bindings should
   act on it.
4. Bind them in `default_keymap.toml`.

Headway is the worked example: `crates/notedeck_headway/src/ui_tools.rs`
(K1 and K4 of headway:notedeck/daring-account-sphere). The registry lists UI
tools as `ToolKind::Ui`, so agent backends skip them unless they opt in.

### Where a user's keymap lives

The account's overrides are a NIP-78 app-data note, kind 30078 with
`d = "notedeck/keymap"`, written PNS-encrypted through
`notedeck::write_private_note` (`src/keymap/user.rs`). It syncs to the
account's other devices, and the chrome picks a new revision up live
(`src/chrome/user_keymap.rs`). A revision that doesn't merge leaves the
defaults in use, and `Chrome::keymap_error` says why.

It is edited inside notedeck: the side menu's **Keymap** item, or the
`chrome_edit_keymap` tool, opens Notebook's keymap editor on the stored
overrides (or `KEYMAP_TEMPLATE`, a commented example, if there are none).
Its Save, or Ctrl+S, raises `chrome_save_keymap { toml }`. The chrome merges
and saves it through `Chrome::save_user_keymap` and answers with
`Notebook::keymap_saved`, so a refusal shows its reason in the editor and
nothing is written (`src/chrome/keymap_editor.rs`).
`chrome_edit_keymap` has no default key; bind one in your own keymap.
