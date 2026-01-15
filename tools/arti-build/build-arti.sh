#!/bin/bash
#
# Build script for Arti Android native library
#
# Prerequisites:
#   - cargo-ndk: cargo install cargo-ndk
#   - Android NDK 25+: Set ANDROID_NDK_HOME
#   - Rust targets: rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
#
# Usage:
#   ./build-arti.sh           # Build all architectures
#   ./build-arti.sh --release # Build ARM64 only (production)
#   ./build-arti.sh --clean   # Clean build

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_DIR="$PROJECT_ROOT/crates/notedeck_chrome/android/app/src/main/jniLibs"

# Minimum SDK version
MIN_SDK=21

# Target architectures
ALL_TARGETS=(
    "arm64-v8a:aarch64-linux-android"
    "armeabi-v7a:armv7-linux-androideabi"
    "x86_64:x86_64-linux-android"
    "x86:i686-linux-android"
)

# Release mode only builds ARM64
RELEASE_TARGETS=(
    "arm64-v8a:aarch64-linux-android"
)

# Parse arguments
CLEAN=false
RELEASE_ONLY=false
while [[ $# -gt 0 ]]; do
    case $1 in
        --clean)
            CLEAN=true
            shift
            ;;
        --release)
            RELEASE_ONLY=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--clean] [--release]"
            exit 1
            ;;
    esac
done

# Check prerequisites
if ! command -v cargo-ndk &> /dev/null; then
    echo "Error: cargo-ndk not found. Install with: cargo install cargo-ndk"
    exit 1
fi

if [ -z "$ANDROID_NDK_HOME" ]; then
    # Try common locations
    if [ -d "$HOME/Library/Android/sdk/ndk" ]; then
        ANDROID_NDK_HOME=$(ls -d "$HOME/Library/Android/sdk/ndk"/*/ 2>/dev/null | tail -1)
    elif [ -d "/opt/homebrew/share/android-commandlinetools/ndk" ]; then
        ANDROID_NDK_HOME=$(ls -d "/opt/homebrew/share/android-commandlinetools/ndk"/*/ 2>/dev/null | tail -1)
    fi

    if [ -z "$ANDROID_NDK_HOME" ]; then
        echo "Error: ANDROID_NDK_HOME not set and NDK not found in common locations"
        exit 1
    fi
    echo "Using NDK: $ANDROID_NDK_HOME"
    export ANDROID_NDK_HOME
fi

# Clean if requested
if [ "$CLEAN" = true ]; then
    echo "Cleaning build artifacts..."
    rm -rf "$SCRIPT_DIR/target"
    rm -rf "$OUTPUT_DIR"
fi

# Select targets
if [ "$RELEASE_ONLY" = true ]; then
    TARGETS=("${RELEASE_TARGETS[@]}")
    echo "Building ARM64 only (release mode)"
else
    TARGETS=("${ALL_TARGETS[@]}")
    echo "Building all architectures"
fi

# Build for each target
cd "$SCRIPT_DIR"

for target_pair in "${TARGETS[@]}"; do
    ABI="${target_pair%%:*}"
    TARGET="${target_pair##*:}"

    echo ""
    echo "=========================================="
    echo "Building for $ABI ($TARGET)"
    echo "=========================================="

    cargo ndk \
        --target "$TARGET" \
        --platform "$MIN_SDK" \
        build --release

    # Create output directory
    mkdir -p "$OUTPUT_DIR/$ABI"

    # Copy and strip the library
    SRC="$SCRIPT_DIR/target/$TARGET/release/libarti_android.so"
    DST="$OUTPUT_DIR/$ABI/libarti_android.so"

    if [ -f "$SRC" ]; then
        cp "$SRC" "$DST"

        # Strip debug symbols using NDK's llvm-strip
        STRIP="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin/llvm-strip"
        if [ ! -f "$STRIP" ]; then
            STRIP="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-aarch64/bin/llvm-strip"
        fi
        if [ ! -f "$STRIP" ]; then
            STRIP="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip"
        fi

        if [ -f "$STRIP" ]; then
            "$STRIP" "$DST"
        else
            echo "Warning: llvm-strip not found, skipping strip"
        fi

        SIZE=$(ls -lh "$DST" | awk '{print $5}')
        echo "Built: $DST ($SIZE)"
    else
        echo "Error: Build output not found: $SRC"
        exit 1
    fi
done

echo ""
echo "=========================================="
echo "Build complete!"
echo "=========================================="
echo ""
echo "Output files:"
for target_pair in "${TARGETS[@]}"; do
    ABI="${target_pair%%:*}"
    if [ -f "$OUTPUT_DIR/$ABI/libarti_android.so" ]; then
        SIZE=$(ls -lh "$OUTPUT_DIR/$ABI/libarti_android.so" | awk '{print $5}')
        echo "  $OUTPUT_DIR/$ABI/libarti_android.so ($SIZE)"
    fi
done
