# Damus Android

A multiplatform nostr client. Works on android and desktop

Alpha! WIP!

<img src="https://cdn.jb55.com/s/bebeeadf7001fae1.png" height="500px" />

## Compiling

The `shell.nix` provides a reproducible build environment for android and rust. I recommend using [direnv][direnv] to load this environment when you `cd` into the directory.

Once you have your dev shell setup, you can build with this command:

```bash
$ cargo apk run --release 
```

This will build and run the app on your android device. If you don't have the `aarch64-linux-android` rust target yet, you can install it with:

```
$ rustup target add aarch64-linux-android
```

You can also just type

```bash
$ cargo run --release
```

To run the multiplatform desktop version of the app called NoteDeck.
 

[direnv]: https://direnv.net/
