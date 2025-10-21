# `robius-android-env`

[![Latest Version](https://img.shields.io/crates/v/robius-android-env.svg)](https://crates.io/crates/robius-android-env)
[![Docs](https://docs.rs/robius-android-env/badge.svg)](https://docs.rs/robius-android-env/latest/robius_android_env/)
[![Project Robius Matrix Chat](https://img.shields.io/matrix/robius-general%3Amatrix.org?server_fqdn=matrix.org&style=flat&logo=matrix&label=Project%20Robius%20Matrix%20Chat&color=B7410E)](https://matrix.to/#/#robius:matrix.org)

This crate provides easy Rust access to Android state (native Java objects) managed by UI toolkits.

# Usage of this crate
This crate exists for two kinds of downstream users:
1. The UI toolkit that exposes its key internal states that hold
   the current Android activity being displayed and the Java VM / JNI environment.
   Either the UI toolkit or the app itself should set these states on startup,
   either by using [ndk-context] or by activating a feature for a specific UI toolkit.
2. The platform feature "middleware" crates that need to access the current activity
   and JNI environment from Rust code in order to interact with the Android platform.

## Supported UI toolkits
* [Makepad]: enable the `makepad` Cargo feature.
* UI toolkits compatible with [ndk-context]: supported by default.
* Others coming soon! (in the meantime, see below)

## Usage of this crate for other UI toolkits
For any other UI toolkits that support [ndk-context], you don't need to enable any cargo features.
However, either your application code or the UI toolkit must manually initialize the Android context
owned by [ndk-context], i.e., by invoking [`initialize_android_context()`](https://docs.rs/ndk-context/latest/ndk_context/fn.initialize_android_context.html).
Some UI toolkits automatically do this for you, typically via the [ndk-glue] crate.

[Makepad]: https://github.com/makepad/makepad/
[ndk-context]: https://docs.rs/ndk-context/latest/ndk_context/
[ndk-glue]: https://crates.io/crates/ndk-glue
