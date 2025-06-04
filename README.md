# Notedeck

[![CI](https://github.com/damus-io/notedeck/actions/workflows/rust.yml/badge.svg)](https://github.com/damus-io/notedeck/actions/workflows/rust.yml) 
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/damus-io/notedeck)

A modern, multiplatform Nostr client built with Rust. Notedeck provides a feature-rich experience for interacting with the Nostr protocol on both desktop and Android platforms.

<p align="center">
  <img src="https://cdn.jb55.com/s/6130555f03db55b2.png" alt="Notedeck Desktop Screenshot" width="700">
</p>

## ✨ Features

- **Multi-column Layout**: TweetDeck-style interface for viewing different Nostr content
- **Dave AI Assistant**: AI-powered assistant that can search and analyze Nostr content
- **Profile Management**: View and edit Nostr profiles
- **Media Support**: View and upload images with GIF support
- **Lightning Integration**: Zap (tip) content creators with Bitcoin Lightning
- **Cross-platform**: Works on desktop (Linux, macOS, Windows) and Android

## 📱 Mobile Support

Notedeck runs smoothly on Android devices with a responsive interface:

<p align="center">
  <img src="https://cdn.jb55.com/s/bebeeadf7001fae1.png" alt="Notedeck Android Screenshot" height="500px">
</p>

## 🏗️ Project Structure

```
notedeck
├── crates
│   ├── notedeck           - Core library with shared functionality
│   ├── notedeck_chrome    - UI container and navigation framework
│   ├── notedeck_columns   - TweetDeck-style column interface
│   ├── notedeck_dave      - AI assistant for Nostr
│   ├── notedeck_ui        - Shared UI components
│   └── tokenator          - String token parsing library
```

## 🚀 Getting Started

### Desktop

To run on desktop platforms:

```bash
# Development build
cargo run -- --debug

# Release build
cargo run --release
```

### Android

For Android devices:

```bash
# Install required target
rustup target add aarch64-linux-android

# Build and install on connected device
cargo apk run --release -p notedeck_chrome
```

### Android Emulator

1. Install [Android Studio](https://developer.android.com/studio)
2. Open 'Device Manager' and create a device with API level `34` and ABI `arm64-v8a`
3. Start the emulator
4. Run: `cargo apk run --release -p notedeck_chrome`

## 🧪 Development

### Android Configuration

Customize Android views for testing:

1. Copy `example-android-config.json` to `android-config.json`
2. Run `make push-android-config` to deploy to your device

### Setting Up Developer Environment

```bash
./scripts/dev_setup.sh
```

This adds pre-commit hooks for proper code formatting.

## 📚 Documentation

Detailed developer documentation is available in each crate:

- [Notedeck Core](./crates/notedeck/DEVELOPER.md)
- [Notedeck Chrome](./crates/notedeck_chrome/DEVELOPER.md)
- [Notedeck Columns](./crates/notedeck_columns/DEVELOPER.md)
- [Dave AI Assistant](./crates/notedeck_dave/docs/README.md)
- [UI Components](./crates/notedeck_ui/docs/components.md)

## 🔄 Release Status

Notedeck is currently in **ALPHA** status. For the latest changes, see the [CHANGELOG](./CHANGELOG.md).

## 🤝 Contributing

Contributions are welcome! Please check the developer documentation and follow these guidelines:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 🔒 Security

For security issues, please refer to our [Security Policy](./SECURITY.md).

## 📄 License

This project is licensed under the GPL - see license information in individual crates.

## 👥 Authors

- William Casarin <jb55@jb55.com>
- kernelkind <kernelkind@gmail.com>
- And [contributors](https://github.com/damus-io/notedeck/graphs/contributors)
