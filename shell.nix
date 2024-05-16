{ android ? "https://github.com/tadfisher/android-nixpkgs/archive/refs/tags/2024-04-02.tar.gz"
, use_android ? true
, android_emulator ? false
}:
with import <nixpkgs>
{
  overlays = [
    (import (builtins.fetchTarball {
      url = "https://github.com/oxalica/rust-overlay/archive/master.tar.gz";
    }))
  ];
  config = {
    android_sdk.accept_license = use_android;
    allowUnfree = use_android;
  };
};

let
  x11libs = lib.makeLibraryPath [ xorg.libX11 xorg.libXcursor xorg.libXrandr xorg.libXi libglvnd vulkan-loader vulkan-validation-layers libxkbcommon ];
  rustc = (rust-bin.fromRustupToolchainFile ./rust-toolchain).override {
    targets = [ ] ++
      (lib.optionals (stdenv.isLinux && use_android) [
        "aarch64-linux-android"
      ]) ++
      (lib.optionals (stdenv.isLinux && stdenv.isx86_64 && use_android && android_emulator) [
        "x86_64-linux-android"
      ]) ++
      (lib.optionals (stdenv.isLinux && stdenv.isx86_64) [
        "x86_64-unknown-linux-gnu"
      ]) ++
      (lib.optionals (stdenv.isLinux && !stdenv.isx86_64) [
        "aarch64-unknown-linux-gnu"
      ]) ++
      (lib.optionals (stdenv.isDarwin && stdenv.isx86_64) [
        "x86_64-apple-darwin"
      ]) ++
      (lib.optionals (stdenv.isDarwin && !stdenv.isx86_64) [
        "aarch64-apple-darwin"
      ])
    ;
  };
in
mkShell ({
  nativeBuildInputs = [
    rustc
    #cargo-udeps
    #cargo-edit
    #cargo-watch
    # rustup
    # rustfmt
    libiconv
    pkg-config
    #cmake
    fontconfig
    #brotli
    #wabt
    #gdb
    #heaptrack
  ] ++ lib.optionals use_android [
    jre
    openssl
    libiconv
    cargo-apk
  ] ++ lib.optional stdenv.isDarwin [
    darwin.apple_sdk.frameworks.Security
    darwin.apple_sdk.frameworks.OpenGL
    darwin.apple_sdk.frameworks.CoreServices
    darwin.apple_sdk.frameworks.AppKit
  ];

} // (
  lib.optionalAttrs (!stdenv.isDarwin) {
    LD_LIBRARY_PATH = "${x11libs}";
  }
) // (
  lib.optionalAttrs use_android (
    let
      android-nixpkgs = callPackage (fetchTarball android) { };
      ndk-version = "24.0.8215888";

      android-sdk = android-nixpkgs.sdk
        (sdkPkgs: with sdkPkgs; [
          cmdline-tools-latest
          build-tools-34-0-0
          platform-tools
          platforms-android-30
          ndk-24-0-8215888
        ] ++
        (lib.optionals android_emulator [ emulator ]) ++
        (lib.optionals (android_emulator && stdenv.isx86_64) [ system-images-android-34-google-apis-x86-64 ]) ++
        (lib.optionals (android_emulator && !stdenv.isx86_64) [ system-images-android-34-google-apis-arm64-v8a ]));

      android-sdk-path = "${android-sdk.out}/share/android-sdk";
      android-ndk-path = "${android-sdk-path}/ndk/${ndk-version}";

    in
    {
      buildInputs = [ android-sdk ];
      ANDROID_NDK_ROOT = android-ndk-path;
    }
  )
))
