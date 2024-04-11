{ pkgs ? import <nixpkgs> { }
, android ? fetchTarball "https://github.com/tadfisher/android-nixpkgs/archive/refs/tags/2024-04-02.tar.gz"
, use_android ? true }:
with pkgs;

let
  x11libs = lib.makeLibraryPath [ xorg.libX11 xorg.libXcursor xorg.libXrandr xorg.libXi libglvnd vulkan-loader vulkan-validation-layers libxkbcommon ];
  android-nixpkgs = callPackage android { };
  ndk-version = "24.0.8215888";

  android-sdk = android-nixpkgs.sdk (sdkPkgs: with sdkPkgs; [
    cmdline-tools-latest
    build-tools-34-0-0
    platform-tools
    platforms-android-30
    emulator
    ndk-24-0-8215888
  ]);

  android-sdk-path = "${android-sdk.out}/share/android-sdk";
  android-ndk-path = "${android-sdk-path}/ndk/${ndk-version}";

in
mkShell ({
  buildInputs = [] ++ pkgs.lib.optional use_android [
    android-sdk
  ];
  nativeBuildInputs = [
    #cargo-udeps
    #cargo-edit
    #cargo-watch
    rustup
    rustfmt
    libiconv
    pkg-config
    #cmake
    fontconfig
    #brotli
    #wabt
    #gdb
    #heaptrack
  ] ++ lib.optional use_android [
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

  ANDROID_NDK_ROOT = android-ndk-path;
} // (if !stdenv.isDarwin then {
  LD_LIBRARY_PATH="${x11libs}";
} else {}))
