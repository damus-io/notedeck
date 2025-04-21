# NoteDeck UI

UI component library for NoteDeck - a Nostr client built with EGUI.

## Overview

The `notedeck_ui` crate provides a set of reusable UI components for building a Nostr client. It offers consistent styling, behavior, and rendering of Nostr-specific elements like notes, profiles, mentions, and media content.

This library is built on top of [egui](https://github.com/emilk/egui), a simple, fast, and highly portable immediate mode GUI library for Rust.

## Features

- 📝 Note display with rich content, media, and interactions
- 👤 Profile components (display name, pictures, banners)
- 🔗 Mention system with hover previews
- 🖼️ Image handling with caching and lazy loading
- 📺 GIF playback support
- 💸 Zap interactions (Bitcoin Lightning tips)
- 🎨 Theming and consistent styling

## Components

### Notes

The `NoteView` widget is the core component for displaying Nostr notes:

```rust
// Example: Render a note
let mut note_view = NoteView::new(
    note_context,
    current_account,
    &note, 
    NoteOptions::default()
);

ui.add(&mut note_view);
```

`NoteView` supports various display options:

```rust
// Create a preview style note
note_view
    .preview_style()       // Apply preview styling
    .textmode(true)        // Use text-only mode
    .actionbar(false)      // Hide action bar
    .small_pfp(true)       // Use small profile picture
    .note_previews(false)  // Disable nested note previews
    .show(ui);
```

### Profiles

Profile components include profile pictures, banners, and display names:

```rust
// Display a profile picture
ui.add(ProfilePic::new(images_cache, profile_picture_url).size(48.0));

// Display a profile preview
ui.add(ProfilePreview::new(profile_record, images_cache));
```

### Mentions

The mention component links to user profiles:

```rust
// Display a mention with hover preview
let mention_response = Mention::new(ndb, img_cache, txn, pubkey)
    .size(16.0)            // Set text size
    .selectable(true)      // Allow selection
    .show(ui);

// Handle click actions
if let Some(action) = mention_response.inner {
    // Handle profile navigation
}
```

### Media

Support for images, GIFs, and other media types:

```rust
// Render an image
render_images(
    ui,
    img_cache,
    image_url,
    ImageType::Content,
    cache_type,
    on_loading_callback,
    on_error_callback,
    on_success_callback
);
```

## Styling

The UI components adapt to the current theme (light/dark mode) and use consistent styling defined in the `colors.rs` module:

```rust
// Color constants
pub const ALMOST_WHITE: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const MID_GRAY: Color32 = Color32::from_rgb(0xbd, 0xbd, 0xbd);
pub const PINK: Color32 = Color32::from_rgb(0xE4, 0x5A, 0xC9);
pub const TEAL: Color32 = Color32::from_rgb(0x77, 0xDC, 0xE1);
```

## Dependencies

This crate depends on:
- `egui` - Core UI library
- `egui_extras` - Additional widgets and functionality
- `ehttp` - HTTP client for fetching content
- `nostrdb` - Nostr database and types
- `enostr` - Nostr protocol implementation
- `image` - Image processing library
- `poll-promise` - Async promise handling
- `tokio` - Async runtime
