# `robius-open`

[![Latest Version](https://img.shields.io/crates/v/robius-open.svg)](https://crates.io/crates/robius-open)
[![Docs](https://docs.rs/robius-open/badge.svg)](https://docs.rs/robius-open/latest/robius_open/)
[![Project Robius Matrix Chat](https://img.shields.io/matrix/robius-general%3Amatrix.org?server_fqdn=matrix.org&style=flat&logo=matrix&label=Project%20Robius%20Matrix%20Chat&color=B7410E)](https://matrix.to/#/#robius:matrix.org)

This crate provides easy Rust interfaces to open URIs across multiple platforms, including:
- macOS (via `NSWorkspace`)
- Android (via `android/content/Intent`)
- Linux (via `xdg-open`)
- Windows (via `start`)
- iOS (via `UIApplication`)

URIs take many different forms: URLs (`http://`), `tel:`, `mailto:`, `file://`, and more (see the [official list of schemes](https://www.iana.org/assignments/uri-schemes/uri-schemes.xhtml)).

## Examples

```rust
use robius_open::Uri;
Uri::new("tel:+61 123 456 789")
   .open()
   .expect("failed to open telephone URI");
```

```rust
use robius_open::Uri;
Uri::new("http://www.google.com")
   .open()
   .expect("failed to open URL");
```


## Android usage
To use this crate on Android, you must add the following to your app manifest:
```xml
<uses-permission android:name="android.permission.QUERY_ALL_PACKAGES"
   tools:ignore="QueryAllPackagesPermission" />

<queries>
   <intent>
      <action android:name="android.intent.action.MAIN" />
   </intent>
</queries>
```
or alternatively, disable the `android-result` feature.

However, disabling this feature will make `Uri::open()` always return `Ok`, regardless of whether the URI was successfully opened.
