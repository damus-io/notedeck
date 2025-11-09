# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Notedeck is a modern, multiplatform Nostr client built with Rust, featuring a TweetDeck-style interface. It runs on desktop (Linux, macOS, Windows) and Android, with a focus on performance through the custom nostrdb embedded database.

**Current Status**: BETA (v0.7.1)
**Main Branch**: master
**License**: GPL

## Build Commands

### Desktop Development

#### System Dependencies
**FFmpeg** is required for video playback on desktop platforms (Linux, macOS, Windows):

```bash
# macOS (Homebrew)
brew install ffmpeg

# Ubuntu/Debian
sudo apt-get install libavcodec-dev libavformat-dev libavutil-dev libavdevice-dev

# Fedora
sudo dnf install ffmpeg-devel

# Windows (via vcpkg)
vcpkg install ffmpeg
```

**Note**: Video playback is desktop-only and disabled on Android/WASM builds.

#### Build Commands
```bash
# Development build with debug logging
cargo run -- --debug

# Release build
cargo run --release

# Size-optimized build
cargo build --profile small
```

### CI-equivalent Checks
```bash
# Run all CI checks locally (recommended before committing)
./check

# Or run manually:
cargo check --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -W clippy::all
cargo test --workspace --all-targets --all-features
cargo test --workspace --doc
```

### Android Development
```bash
# Install Android target (first time only)
rustup target add aarch64-linux-android
cargo install cargo-ndk

# Build and run on connected device/emulator
cargo apk run --release -p notedeck_chrome

# Using Makefile:
make jni          # Build JNI libraries
make apk          # Build APK
make android      # Install and run with live logcat
make android-tracy # Build with Tracy profiling support

# Push custom Android config to device
make push-android-config
```

### Testing
```bash
# Run all tests
cargo test --workspace --all-targets --all-features

# Run doc tests
cargo test --workspace --doc

# Run specific crate tests
cargo test -p notedeck_columns

# Run tests matching a pattern
cargo test timeline
```

### Development Setup
```bash
# Install git hooks and toolchains
./scripts/dev_setup.sh
```

## Architecture Overview

### Workspace Structure

Notedeck uses a **workspace-based architecture** with 9 crates:

**Core Libraries:**
- `crates/notedeck` - Core library with shared functionality (accounts, media caching, i18n, storage, Lightning integration)
- `crates/notedeck_ui` - Shared UI components (note rendering, profiles, media display, zaps)
- `crates/enostr` - Nostr protocol implementation with relay management
- `crates/tokenator` - String token parsing library

**Application Crates:**
- `crates/notedeck_chrome` - Browser chrome/container, main binary, navigation framework (main entry point)
- `crates/notedeck_columns` - TweetDeck-style column interface (primary app experience)
- `crates/notedeck_dave` - AI assistant with 3D WebGPU avatar
- `crates/notedeck_clndash` - Core Lightning dashboard (experimental)
- `crates/notedeck_notebook` - Additional app component

### Key Technologies

- **UI Framework**: egui (immediate mode GUI) with eframe
- **Graphics**: WGPU rendering pipeline
- **Database**: nostrdb - custom embedded database optimized for Nostr
- **Async Runtime**: tokio with multi-threading
- **Media**: rsmpeg (desktop video decoding with hardware acceleration), rodio (audio playback), image crate (JPEG/PNG/WebP)
- **Lightning**: nwc (Nostr Wallet Connect), lightning-invoice

### Important Patterns

1. **Immediate Mode UI**: egui uses immediate mode paradigm - UI is rebuilt every frame
2. **Promise-based Async**: Uses poll-promise for async operations in egui context
3. **Caching Strategy**: Efficient image and media caching in `crates/notedeck/src/media/`
4. **Job Pool Pattern**: Background task execution via `crates/notedeck/src/job_pool.rs`
5. **Platform Abstraction**: Extensive use of `#[cfg(target_os = "...")]` for platform-specific code

### Key Directories

- `crates/notedeck/src/account/` - Account management and authentication
- `crates/notedeck/src/media/` - Image/video caching and processing
- `crates/notedeck_ui/src/` - Reusable UI widgets and components
- `crates/notedeck_columns/src/timeline/` - Timeline rendering and management
- `crates/enostr/src/relay/` - Relay connection and event handling
- `assets/` - Icons, images, and other static resources

## Custom egui Fork

**IMPORTANT**: This project uses a **custom fork of egui** from damus-io, not the upstream version. All egui crates are patched in `Cargo.toml`:

```toml
[patch.crates-io]
egui = { git = "https://github.com/damus-io/egui", rev = "e05638c40ef734312b3b3e36397d389d0a78b10b" }
eframe = { git = "https://github.com/damus-io/egui", rev = "e05638c40ef734312b3b3e36397d389d0a78b10b" }
# ... and others
```

Do not suggest upgrading to upstream egui without understanding the custom patches.

## Platform-Specific Code

### Desktop-Only Features
Video playback with audio (ffmpeg) is desktop-only:
```rust
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
```

### Android-Specific
- JNI bindings in android-specific code
- Uses game-activity instead of native-activity
- Custom android-config.json for testing different views

### WASM Considerations
Some features are disabled on WASM (file I/O, native video decoding)

## Recent Major Changes

- **Video Playback** (commit 69c0289): Added inline video playback with audio using ffmpeg
- **Note Metadata**: Rolling statistics display with smooth animations
- **Platform Independence**: Fixed nostrdb Windows time overflow issues

## Code Style

- **Formatting**: Use `cargo fmt` (4-space indents, rustfmt defaults)
- **Linting**: CI treats clippy warnings as errors (`-D warnings`)
- **Naming**: snake_case for modules/files, UpperCamelCase for types, SCREAMING_SNAKE_CASE for constants
- **Imports**: Prefer explicit `use crate::...` over glob imports
- **Pre-commit Hook**: `scripts/pre_commit_hook.sh` runs formatting checks

## Security Considerations

- Never commit actual keys (damus.keystore is a placeholder)
- Keep android-config.json in .gitignore (local testing only)
- Review SECURITY.md for security reporting

## Testing Notes

- 20+ test modules across the workspace
- Test utilities in `crates/notedeck_columns/src/test_utils.rs`
- Use `tempfile` for filesystem tests
- Platform-specific tests use conditional compilation

## Common Development Tasks

### Adding a New UI Component
1. Place shared components in `crates/notedeck_ui/src/`
2. App-specific components go in the relevant app crate (e.g., `notedeck_columns`)
3. Follow egui immediate mode patterns
4. Consider mobile constraints (touch targets, smaller screens)

### Working with Nostr Events
1. Use nostrdb for event storage/queries (not manual parsing)
2. Event filters are in `crates/notedeck/src/filter.rs`
3. Relay management is in `crates/enostr/src/relay/`

### Adding Media Support
1. Caching logic goes in `crates/notedeck/src/media/`
2. Desktop video uses ffmpeg (not available on Android/WASM)
3. Use the job pool for background processing

### Internationalization
1. Translations managed via Crowdin
2. Fluent files for i18n strings
3. Manager in `crates/notedeck/src/i18n/manager.rs`

## CI/CD

GitHub Actions workflow (`.github/workflows/rust.yml`) runs:
1. **Lint**: rustfmt + clippy on ubuntu-22.04
2. **Android Check**: cargo-ndk build verification
3. **Multi-platform Tests**: Linux, macOS, Windows
4. **Packaging**: RPM, DEB (Linux), DMG (macOS), Inno Setup (Windows)
5. **Deployment**: SFTP upload on master/ci branches

## Profiling

- **puffin**: Built-in profiler (optional feature)
- **tracy**: Advanced profiling support via `--features tracy`
- Android tracy: `make android-tracy` (forwards port 8086)

## Documentation

Comprehensive developer docs available in:
- `crates/notedeck/DEVELOPER.md`
- `crates/notedeck_chrome/DEVELOPER.md`
- `crates/notedeck_columns/DEVELOPER.md`
- `crates/notedeck_dave/docs/README.md`
- `crates/notedeck_ui/docs/components.md`

Consult these for crate-specific architecture details.
