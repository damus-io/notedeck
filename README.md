# Damus

A multiplatform nostr client. Works on android and desktop

Alpha! WIP!

## Desktop

The desktop client is called notedeck:

![notedeck](https://cdn.jb55.com/s/notedeck-2024-04.png)

## Android

Look it actually runs on android!

<img src="https://cdn.jb55.com/s/bebeeadf7001fae1.png" height="500px" />

## Usage

You can customize the columns by passing them as command-line arguments. This is only for testing and will likely change.

```bash
$ ./target/release/notedeck "$(cat queries/timeline.json)" "$(cat queries/notifications.json)"
```

# Developer Setup

## Desktop (Linux/MacOS, Windows?)

First, install [nix][nix] if you don't have it.

The `shell.nix` provides a reproducible build environment for android and rust. I recommend using [direnv][direnv] to load this environment when you `cd` into the directory.

If you don't have [direnv][direnv], enter the dev shell via:

```bash
$ nix-shell
```

Once you have your dev shell setup, you can build with this command:

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
[nix]: https://nixos.org/download/
