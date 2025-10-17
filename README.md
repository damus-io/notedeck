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
- **Livestream Browser**: View-only NIP-53 livestream directory with status, schedules, and participant details
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

## 🛠️ Troubleshooting

### Linux inline playback shows black video while audio works

On some Linux systems the GPU video acceleration stack (VAAPI) can misbehave. When GStreamer picks the VAAPI decoder it may return a frozen desktop frame instead of the stream, so both Notedeck and `gst-launch-1.0` render black video even though the audio continues.

Workaround: force GStreamer to stay on software decoding before launching Notedeck.

```bash
export GST_VAAPI_DISABLE=1
export LIBVA_DRIVER_NAME=dummy
export GST_PLUGIN_FEATURE_RANK=vaapidecodebin:0,vaapih264dec:0,vaapipostproc:0,vaapisink:0

RUST_LOG=notedeck_livestreams=debug \
cargo run -p notedeck_chrome --release --features inline-playback -- \
  --debug --datapath ./target/
```

You can use the same environment variables with `gst-launch-1.0 playbin … video-sink='videoconvert ! ximagesink'` to confirm the stream renders correctly. Android builds do **not** use VAAPI—they rely on the platform's MediaCodec decoders instead—so this issue is limited to Linux desktops.

## 🔄 Release Status

Notedeck is currently in **BETA** status. For the latest changes, see the [CHANGELOG](./CHANGELOG.md).

## Future

Notedeck allows for app development built on top of the performant, built specifically for nostr database [nostrdb][nostrdb]. An example app written on notedeck is [Dave](./crates/notedeck_dave)

Building on notedeck dev documentation is also on the roadmap.

## 🤝 Contributing

### Developers

Contributions are welcome! Please check the developer documentation and follow these guidelines:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

### Translators

Help us bring Notedeck to non-English speakers!

Request to join the Notedeck translations team through [Crowdin](https://crowdin.com/project/notedeck).

If you do not have a Crowdin account, sign up for one.
If you do not see your language, please request it in Crowdin.

## 🔒 Security

For security issues, please refer to our [Security Policy](./SECURITY.md).

## 📄 License

This project is licensed under the GPL - see license information in individual crates.

## 👥 Authors

- William Casarin <jb55@jb55.com>
- kernelkind <kernelkind@gmail.com>
- And [contributors](https://github.com/damus-io/notedeck/graphs/contributors)


[nostrdb]: https://github.com/damus-io/nostrdb
