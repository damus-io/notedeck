# NoteDeck UI Component Guide

This guide provides detailed documentation for the major UI components in the NoteDeck UI library.

## Table of Contents

- [Notes](#notes)
  - [NoteView](#noteview)
  - [NoteContents](#notecontents)
  - [NoteOptions](#noteoptions)
- [Profiles](#profiles)
  - [ProfilePic](#profilepic)
  - [ProfilePreview](#profilepreview)
- [Mentions](#mentions)
- [Media Handling](#media-handling)
  - [Images](#images)
  - [GIF Animation](#gif-animation)
- [Widgets & Utilities](#widgets--utilities)

## Notes

### NoteView

The `NoteView` component is the main container for displaying Nostr notes, handling the layout of profile pictures, author information, content, and interactive elements.

#### Usage

```rust
let mut note_view = NoteView::new(
    note_context,  // NoteContext with DB, cache, etc.
    current_acc,   // Current user account (Option<KeypairUnowned>)
    &note,         // Reference to Note
    options        // NoteOptions (display configuration)
);

// Configure display options
note_view
    .actionbar(true)      // Show/hide action bar
    .small_pfp(false)     // Use small profile picture
    .medium_pfp(true)     // Use medium profile picture
    .wide(false)          // Use wide layout
    .frame(true)          // Display with a frame
    .note_previews(true)  // Enable embedded note previews
    .selectable_text(true); // Allow text selection

// Render the note view
let note_response = note_view.show(ui);

// Handle user actions
if let Some(action) = note_response.action {
    match action {
        NoteAction::Note(note_id) => { /* Note was clicked */ },
        NoteAction::Profile(pubkey) => { /* Profile was clicked */ },
        NoteAction::Reply(note_id) => { /* User clicked reply */ },
        NoteAction::Quote(note_id) => { /* User clicked quote */ },
        NoteAction::Zap(zap_action) => { /* User initiated zap */ },
        NoteAction::Hashtag(tag) => { /* Hashtag was clicked */ },
        NoteAction::Context(ctx_selection) => { /* Context menu option selected */ },
    }
}
```

#### Layouts

`NoteView` supports two main layouts:

1. **Standard Layout** - Default compact display
2. **Wide Layout** - More spacious layout with profile picture on the left

Use the `.wide(true)` option to enable the wide layout.

#### Preview Style

For displaying note previews (e.g., when a note is referenced in another note), use the preview style:

```rust
let mut note_view = NoteView::new(note_context, current_acc, &note, options)
    .preview_style(); // Applies preset options for preview display
```

### NoteContents

`NoteContents` handles rendering the actual content of a note, including text, mentions, hashtags, URLs, and embedded media.

```rust
let mut contents = NoteContents::new(
    note_context,
    current_acc,
    transaction,
    note,
    note_options
);

ui.add(&mut contents);

// Check for content interactions
if let Some(action) = contents.action() {
    // Handle content action (e.g., clicked mention/hashtag)
}
```

### NoteOptions

`NoteOptions` is a bitflag-based configuration system for controlling how notes are displayed:

```rust
// Create with default options
let mut options = NoteOptions::default();

// Or customize from scratch
let mut options = NoteOptions::new(is_universe_timeline);

// Configure options
options.set_actionbar(true);         // Show action buttons
options.set_small_pfp(true);         // Use small profile picture
options.set_medium_pfp(false);       // Don't use medium profile picture
options.set_note_previews(true);     // Enable note previews
options.set_wide(false);             // Use compact layout
options.set_selectable_text(true);   // Allow text selection
options.set_textmode(false);         // Don't use text-only mode
options.set_options_button(true);    // Show options button
options.set_hide_media(false);       // Show media content
options.set_scramble_text(false);    // Don't scramble text
options.set_is_preview(false);       // This is not a preview
```

## Profiles

### ProfilePic

`ProfilePic` displays a circular profile picture with optional border and configurable size.

```rust
// Basic usage
ui.add(ProfilePic::new(img_cache, profile_url));

// Customized
ui.add(
    ProfilePic::new(img_cache, profile_url)
        .size(48.0)
        .border(Stroke::new(2.0, Color32::WHITE))
);

// From profile record
if let Some(profile_pic) = ProfilePic::from_profile(img_cache, profile) {
    ui.add(profile_pic);
}
```

Standard sizes:
- `ProfilePic::default_size()` - 38px
- `ProfilePic::medium_size()` - 32px
- `ProfilePic::small_size()` - 24px

### ProfilePreview

`ProfilePreview` shows a detailed profile card with banner, profile picture, display name, username, and about text.

```rust
// Full preview
ui.add(ProfilePreview::new(profile, img_cache));

// Simple preview
ui.add(SimpleProfilePreview::new(
    Some(profile),  // Option<&ProfileRecord>
    img_cache,
    is_nsec         // Whether this is a full keypair
));
```

## Mentions

The `Mention` component renders a clickable @username reference with hover preview.

```rust
let mention_response = Mention::new(ndb, img_cache, txn, pubkey)
    .size(16.0)        // Text size
    .selectable(false) // Disable text selection
    .show(ui);

// Handle mention click
if let Some(action) = mention_response.inner {
    // Usually NoteAction::Profile
}
```

## Media Handling

### Images

Images are managed through the `render_images` function, which handles loading, caching, and displaying images:

```rust
render_images(
    ui,
    img_cache,
    url,
    ImageType::Content,  // Or ImageType::Profile(size)
    MediaCacheType::Image,
    |ui| {
        // Show while loading
        ui.spinner();
    },
    |ui, error| {
        // Show on error
        ui.label(format!("Error: {}", error));
    },
    |ui, url, img, gifs| {
        // Show successful image
        let texture = handle_repaint(ui, retrieve_latest_texture(url, gifs, img));
        ui.image(texture);
    }
);
```

For profile images, use `ImageType::Profile(size)` to automatically crop, resize, and round the image.

### GIF Animation

GIFs are supported through the animation system. The process for displaying GIFs is:

1. Load and decode GIF in background thread
2. Send frames to UI thread through channels
3. Render frames with timing control

```rust
// Display a GIF
render_images(
    ui,
    img_cache,
    gif_url,
    ImageType::Content,
    MediaCacheType::Gif,
    /* callbacks as above */
);

// Get the current frame texture
let texture = handle_repaint(
    ui, 
    retrieve_latest_texture(url, gifs, renderable_media)
);
```

## Widgets & Utilities

### Username

Displays a user's name with options for abbreviation and color:

```rust
ui.add(
    Username::new(profile, pubkey)
        .pk_colored(true)     // Color based on pubkey
        .abbreviated(16)      // Max length before abbreviation
);
```

### Animations

Use animation helpers for interactive elements:

```rust
// Basic hover animation
let (rect, size, response) = hover_expand(
    ui,
    id,           // Unique ID for the animation
    base_size,    // Base size
    expand_size,  // Amount to expand by
    anim_speed    // Animation speed
);

// Small hover expand (common pattern)
let (rect, size, response) = hover_expand_small(ui, id);

// Advanced helper
let helper = AnimationHelper::new(ui, "animation_name", max_size);
let current_size = helper.scale_1d_pos(min_size);
```

### Pulsing Effects

For elements that need attention:

```rust
// Create pulsing image
let pulsing_image = ImagePulseTint::new(
    &ctx,                   // EGUI Context
    id,                     // Animation ID
    image,                  // Base image
    &[255, 183, 87],        // Tint color
    alpha_min,              // Minimum alpha
    alpha_max               // Maximum alpha
)
.with_speed(0.35)          // Animation speed
.animate();                // Apply animation

ui.add(pulsing_image);
```

### Context Menus

Create menus for additional actions:

```rust
// Add context menu to any response
response.context_menu(|ui| {
    if ui.button("Copy Link").clicked() {
        ui.ctx().copy_text(url.to_owned());
        ui.close_menu();
    }
});
```

The `NoteContextButton` component provides a standard context menu for notes:

```rust
let resp = ui.add(NoteContextButton::new(note_key));
if let Some(action) = NoteContextButton::menu(ui, resp) {
    // Handle context action
}
```
