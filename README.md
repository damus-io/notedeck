# Damus Notedeck

A multiplatform nostr client. Works on android and desktop

The desktop client is called notedeck:

![notedeck](https://cdn.jb55.com/s/bebeeadf7001fae1.png)

## Android

Look it actually runs on android!

<img src="https://cdn.jb55.com/s/bebeeadf7001fae1.png" height="500px" />

## Usage

```bash
$ ./target/release/notedeck
```

# Developer Setup

## Desktop (Linux/MacOS, Windows?)

If you're running debian-based machine like Ubuntu or ElementaryOS, all you need is to install [rustup] and run `sudo apt install build-essential`.

```bash
$ cargo run --release 
```

## Android

The dev shell should also have all of the android-sdk dependencies needed for development, but you still need the `aarch64-linux-android` rustup target installed:

```
$ rustup target add aarch64-linux-android
```

To run on a real device, just type:

```bash
$ cargo apk run --release
```

## Android Emulator

- Install [Android Studio](https://developer.android.com/studio)
- Open 'Device Manager' in Android Studio
- Add a new device with API level `34` and ABI `arm64-v8a` (even though the app uses 30, the 30 emulator can't find the vulkan adapter, but 34 works fine)
- Start up the emulator

while the emulator is running, run:

```bash
cargo apk run --release
```

The app should appear on the emulator

[direnv]: https://direnv.net/

## Previews

You can preview individual widgets and views by running the preview script:

```bash
./preview RelayView
./preview ProfilePreview
# ... etc
```

When adding new previews you need to implement the Preview trait for your
view/widget and then add it to the `src/ui_preview/main.rs` bin:

```rust
previews!(runner, name,
    RelayView,
    AccountLoginView,
    ProfilePreview,
);
```


## Contributing

Configure the developer environment:

```bash
./scripts/dev_setup.sh
```

This will add the pre-commit hook to your local repository to suggest proper formatting before commits.

[rustup]: https://rustup.rs/
