# NoteDeck UI Developer Documentation

This document provides an in-depth overview of the `notedeck_ui` architecture, components, and guidelines for development.

For a guide on some of our components, check out the [NoteDeck UI Component Guide](./components.md)

## Architecture

The `notedeck_ui` crate is organized into modules that handle different aspects of the Nostr client UI:

```
notedeck_ui
├── anim.rs           - Animation utilities and helpers
├── colors.rs         - Color constants and theme definitions
├── constants.rs      - UI constants (margins, sizes, etc.)
├── gif.rs            - GIF rendering and playback
├── icons.rs          - Icon rendering helpers
├── images.rs         - Image loading, caching, and display
├── lib.rs            - Main export and shared utilities
├── mention.rs        - Nostr mention component (@username)
├── note/             - Note display components
│   ├── contents.rs   - Note content rendering
│   ├── context.rs    - Note context menu
│   ├── mod.rs        - Note view component
│   ├── options.rs    - Note display options
│   └── reply_description.rs - Reply metadata display
├── profile/          - Profile components
│   ├── mod.rs        - Shared profile utilities
│   ├── name.rs       - Profile name display
│   ├── picture.rs    - Profile picture component
│   └── preview.rs    - Profile hover preview
├── username.rs       - Username display component
└── widgets.rs        - Generic widget helpers
```

## Core Components

### NoteView

The `NoteView` component is the primary way to display Nostr notes. It handles rendering the note content, profile information, media, and interactive elements like replies and zaps.

Key design aspects:
- Stateful widget that maintains rendering state through EGUI's widget system
- Configurable display options via `NoteOptions` bitflags
- Support for different layouts (normal and wide)
- Handles nested content (note previews, mentions, hashtags)

```rust
// NoteView creation and display
let mut note_view = NoteView::new(note_context, cur_acc, &note, options);
note_view.show(ui); // Returns NoteResponse with action
```

### Note Actions

The note components use a pattern where user interactions produce `NoteAction` enum values:

```rust
pub enum NoteAction {
    Note(NoteId),              // Note was clicked
    Profile(Pubkey),           // Profile was clicked
    Reply(NoteId),             // Reply button clicked
    Quote(NoteId),             // Quote button clicked
    Hashtag(String),           // Hashtag was clicked
    Zap(ZapAction),            // Zap interaction
    Context(ContextSelection), // Context menu selection
}
```

Actions are propagated up from inner components to the parent UI, which can handle navigation and state changes.

### Media Handling

The media system uses a cache and promise-based loading system:

1. `MediaCache` stores loaded images and animations
2. `fetch_img` retrieves images from disk or network
3. `render_images` handles the loading states and display

For GIFs, the system:
1. Decodes frames using a background thread
2. Sends frames via channels to the UI thread
3. Manages animation timing for playback

## Design Patterns

### Widget Pattern

Components implement the `egui::Widget` trait for integration with EGUI:

```rust
impl egui::Widget for ProfilePic<'_, '_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        render_pfp(ui, self.cache, self.url, self.size, self.border)
    }
}
```

### Builder Pattern

Components often use a builder pattern for configuration:

```rust
let view = NoteView::new(context, account, &note, options)
    .small_pfp(true)
    .wide(true)
    .actionbar(false);
```

### Animation Helper

For interactive elements, the `AnimationHelper` provides standardized hover animations:

```rust
let helper = AnimationHelper::new(ui, "animation_id", max_size);
let current_size = helper.scale_1d_pos(min_size);
```

## Working with Images

Images are handled through different types depending on their purpose:

1. `ImageType::Profile` - For profile pictures (with automatic cropping and rounding)
2. `ImageType::Content` - For general content images

```rust
// Loading and displaying an image
render_images(
    ui,
    img_cache,
    url,
    ImageType::Profile(128), // Size hint
    MediaCacheType::Image,   // Static image or GIF
    |ui| { /* show while loading */ },
    |ui, err| { /* show on error */ },
    |ui, url, img, gifs| { /* show successful load */ },
);
```

## Performance Considerations

1. **Image Caching**: Images are cached both in memory and on disk
2. **Animation Optimization**: GIF frames are decoded in background threads
3. **Render Profiling**: Critical paths use `#[profiling::function]` for tracing
4. **Layout Reuse**: Components cache layout data to prevent recalculation

## Theming

The UI adapts to light/dark mode through EGUI's visuals system:

```rust
// Access current theme
let color = ui.visuals().hyperlink_color;

// Check theme mode
if ui.visuals().dark_mode {
    // Use dark mode resources
} else {
    // Use light mode resources
}
```

## Debugging Tips

1. **EGUI Inspector**: Use `ctx.debug_painter()` to visualize layout bounds
2. **Trace Logging**: Enable trace logs to debug image loading and caching
3. **Animation Debugging**: Set `ANIM_SPEED` to a lower value to slow animations for visual debugging
4. **ID Collisions**: Use unique IDs for animations and state to prevent interaction bugs

## Common Patterns

### Hover Previews

```rust
// For elements with hover previews
let resp = ui.add(/* widget */);
resp.on_hover_ui_at_pointer(|ui| {
    ui.set_max_width(300.0);
    ui.add(ProfilePreview::new(profile, img_cache));
});
```

### Context Menus

```rust
// For elements with context menus
let resp = ui.add(/* widget */);
resp.context_menu(|ui| {
    if ui.button("Menu Option").clicked() {
        // Handle selection
        ui.close_menu();
    }
});
```

## Contributing Guidelines

When contributing to `notedeck_ui`:

1. **Widget Consistency**: Follow established patterns for new widgets
2. **Option Naming**: Keep option names consistent (has_X/set_X pairs)
3. **Performance**: Add profiling annotations to expensive operations
4. **Error Handling**: Propagate errors up rather than handling them directly in UI components
5. **Documentation**: Document public APIs and components with examples
6. **Theme Support**: Ensure components work in both light and dark mode
