{ pkgs ? import <nixpkgs> {}, use_android ? true }:
with pkgs;
let
  x11libs = lib.makeLibraryPath [ xorg.libX11 xorg.libXcursor xorg.libXrandr xorg.libXi libglvnd vulkan-loader vulkan-validation-layers ];
  ndk-version = "24.0.8215888";
  androidComposition = androidenv.composeAndroidPackages {
    includeNDK = true;
    ndkVersions = [ ndk-version ];
    platformVersions = [ "28" "29" "30" ];
    useGoogleAPIs = false;
    #useGoogleTVAddOns = false;
    #includeExtras = [
    #  "extras;google;gcm"
    #];
  };
  androidsdk = androidComposition.androidsdk;
  android-home = "${androidsdk}/libexec/android-sdk";
  ndk-home = "${android-home}/ndk/${ndk-version}";
in

mkShell ({
  nativeBuildInputs = [
    cargo-udeps cargo-edit cargo-watch rustup rustfmt libiconv pkg-config cmake fontconfig
    brotli wabt

    heaptrack

  ] ++ pkgs.lib.optional use_android [ jre openssl libiconv androidsdk cargo-apk ] ;

  LD_LIBRARY_PATH="${x11libs}";
} // (if !use_android then {} else {
  ANDROID_HOME = android-home;
  NDK_HOME = ndk-home;
}))
