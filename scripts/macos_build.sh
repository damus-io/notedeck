#!/bin/bash

set -e  # Exit immediately if a command exits with a non-zero status
set -u  # Treat unset variables as an error
set -o pipefail  # Catch errors in pipelines

# Ensure the script is running in the correct directory
NAME="Notedeck"
REQUIRED_DIR="notedeck"
ARCH=${ARCH:-"aarch64"}
TARGET=${TARGET:-${ARCH}-apple-darwin}
CURRENT_DIR=$(basename "$PWD")

if [ "$CURRENT_DIR" != "$REQUIRED_DIR" ]; then
    echo "Error: This script must be run from the '$REQUIRED_DIR' directory."
    exit 1
fi

# Ensure all required variables are set
REQUIRED_VARS=(NOTEDECK_APPLE_RELEASE_CERT_ID NOTEDECK_RELEASE_APPLE_ID NOTEDECK_APPLE_APP_SPECIFIC_PW NOTEDECK_APPLE_TEAM_ID)
for VAR in "${REQUIRED_VARS[@]}"; do
    if [ -z "${!VAR:-}" ]; then
        echo "Error: Required variable '$VAR' is not set." >&2
        exit 1
    fi
done

# Ensure required tools are installed
if ! command -v cargo &> /dev/null; then
    echo "Error: cargo is not installed." >&2
    exit 1
fi

if ! command -v xcrun &> /dev/null; then
    echo "Error: xcrun is not installed." >&2
    exit 1
fi

if ! command -v create-dmg &> /dev/null; then
    echo "Error: create-dmg is not installed." >&2
    exit 1
fi

# Build the .app bundle. `auto-update` is on so the shipped app can update
# itself from the signed tarball made below. The dev bundle (`make macos-app`)
# leaves it off: an unsigned local bundle should not replace itself.
FEATURES=${FEATURES:-dave,messages,auto-update}
echo "Building .app bundle (features: $FEATURES)..."
cargo bundle -p notedeck_chrome --release --features "$FEATURES" --target $TARGET

# Add the EventKit calendar usage descriptions, but only when Horizon — which
# owns the calendar mirror — is compiled in. macOS TCC requires these strings in
# Info.plist before the app may request calendar access; cargo bundle doesn't
# carry arbitrary keys, so inject them here before signing. Both the legacy and
# the macOS 14+ full-access keys are set so either EventKit path is covered.
if [[ "$FEATURES" == *horizon* ]]; then
    APP_PLIST="target/${TARGET}/release/bundle/osx/$NAME.app/Contents/Info.plist"
    CAL_USAGE="Notedeck mirrors your calendar events into your local nostr database so Horizon can show them."
    echo "Adding calendar usage descriptions to Info.plist..."
    for KEY in NSCalendarsUsageDescription NSCalendarsFullAccessUsageDescription; do
        /usr/libexec/PlistBuddy -c "Add :$KEY string $CAL_USAGE" "$APP_PLIST" 2>/dev/null \
            || /usr/libexec/PlistBuddy -c "Set :$KEY $CAL_USAGE" "$APP_PLIST"
    done
fi

# Sign the app
echo "Codesigning the app..."
codesign \
  --deep \
  --force \
  --verify \
  --options runtime \
  --entitlements entitlements.plist \
  --sign "$NOTEDECK_APPLE_RELEASE_CERT_ID" \
  target/${TARGET}/release/bundle/osx/$NAME.app

# Create a zip for notarization
echo "Creating zip for notarization..."
zip -r notedeck.zip target/${TARGET}/release/bundle/osx/$NAME.app

# Submit for notarization
echo "Submitting for notarization..."
xcrun notarytool submit \
  --apple-id "$NOTEDECK_RELEASE_APPLE_ID" \
  --password "$NOTEDECK_APPLE_APP_SPECIFIC_PW" \
  --team-id "$NOTEDECK_APPLE_TEAM_ID" \
  --wait \
  notedeck.zip

# Staple the notarization
echo "Stapling notarization to the app..."
xcrun stapler staple target/${TARGET}/release/bundle/osx/$NAME.app

echo "Removing notedeck.zip"
rm notedeck.zip

# Pack the stapled bundle as the auto-update asset. The name is
# target_asset_name() in crates/notedeck/src/updater/nostr.rs, which the updater
# matches exactly, and the archive's top-level entry must be Notedeck.app/
# because updater::platform::find_binary_in_dir looks for a *.app at the
# extraction root. BSD tar keeps the bundle's symlinks and exec bits.
# COPYFILE_DISABLE keeps it from adding AppleDouble ._* entries for xattrs:
# the updater unpacks with the Rust tar crate, which would write those out as
# stray files inside the sealed bundle.
UPDATE_TARBALL="packages/notedeck-${ARCH}-macos.tar.gz"
echo "Creating update tarball $UPDATE_TARBALL..."
mkdir -p packages
COPYFILE_DISABLE=1 tar czf "$UPDATE_TARBALL" -C target/${TARGET}/release/bundle/osx $NAME.app

# Prove the update asset survives the archive round trip still signed and
# notarized: unpack it somewhere else and check the extracted bundle the way
# the updater and Gatekeeper will see it.
echo "Verifying the update tarball's bundle..."
VERIFY_DIR=$(mktemp -d)
trap 'rm -rf "$VERIFY_DIR"' EXIT
tar xzf "$UPDATE_TARBALL" -C "$VERIFY_DIR"
codesign --verify --deep --strict --verbose=2 "$VERIFY_DIR/$NAME.app"
spctl --assess --type execute -vv "$VERIFY_DIR/$NAME.app"

# Create the .dmg package
echo "Creating .dmg package..."
mkdir -p packages
create-dmg \
  --window-size 600 400 \
  --app-drop-link 400 100 \
  packages/$NAME-${ARCH}.dmg \
  target/${TARGET}/release/bundle/osx/$NAME.app

echo "Build and signing process completed successfully."
