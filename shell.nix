{ pkgs ? import <nixpkgs> { }
, android ? "https://github.com/tadfisher/android-nixpkgs/archive/refs/tags/2024-04-02.tar.gz"
, use_android ? true
, android_emulator ? false
}:
with pkgs;

let
  x11libs = lib.makeLibraryPath [ xorg.libX11 xorg.libXcursor xorg.libXrandr xorg.libXi libglvnd vulkan-loader vulkan-validation-layers libxkbcommon wayland ];
in
mkShell ({
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

      android-sdk = android-nixpkgs.sdk (sdkPkgs: with sdkPkgs; [
        cmdline-tools-latest
        build-tools-34-0-0
        platform-tools
        platforms-android-30
        ndk-24-0-8215888
      ] ++ lib.optional android_emulator [ emulator ]);

      android-sdk-path = "${android-sdk.out}/share/android-sdk";
      android-ndk-path = "${android-sdk-path}/ndk/${ndk-version}";

    in
    {
      buildInputs = [ android-sdk ];
      ANDROID_NDK_ROOT = android-ndk-path;
    }
  )
))
