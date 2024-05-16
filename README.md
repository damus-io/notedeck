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

The `shell.nix` provides a reproducible build environment, mainly for android but it also includes rust tools if you don't have those installed. It will likely work without nix if you are just looking to do non-android dev and have the rust toolchain already installed. If you decide to use nix, I recommend using [direnv][direnv] to load the nix shell environment when you `cd` into the directory.

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

With Android Studio:

- Install [Android Studio](https://developer.android.com/studio)
- Open 'Device Manager' in Android Studio
- Add a new device with API level `34` and ABI `arm64-v8a` (even though the app uses 30, the 30 emulator can't find the vulkan adapter, but 34 works fine)
- Start up the emulator

Without Android Studio:

```sh
# create emulator
avdmanager create avd -k 'system-images;android-34;google_apis;arm64-v8a' -n notedeck

# start up the emulator
env ANDROID_EMULATOR_WAIT_TIME_BEFORE_KILL=999 emulator -avd notedeck
```

while the emulator is running, run:

```bash
cargo apk run --release
```

The app should appear on the emulator

[direnv]: https://direnv.net/
[nix]: https://nixos.org/download/

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
