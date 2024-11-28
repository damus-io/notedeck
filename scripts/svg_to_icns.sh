#!/bin/bash

# Exit on error
set -e

# Check dependencies
if ! command -v inkscape &> /dev/null; then
    echo "Error: Inkscape is required but not installed. Install it and try again."
    exit 1
fi

if ! command -v iconutil &> /dev/null; then
    echo "Error: iconutil is required but not installed. This tool is available only on macOS."
    exit 1
fi

# Check input arguments
if [ "$#" -ne 2 ]; then
    echo "Usage: $0 input.svg output.icns"
    exit 1
fi

INPUT_SVG=$1
OUTPUT_ICNS=$2
TEMP_DIR=$(mktemp -d)

# Create the iconset directory
ICONSET_DIR="$TEMP_DIR/icon.iconset"
mkdir "$ICONSET_DIR"

# Define sizes and export PNGs
SIZES=(
    "16 icon_16x16.png"
    "32 icon_16x16@2x.png"
    "32 icon_32x32.png"
    "64 icon_32x32@2x.png"
    "128 icon_128x128.png"
    "256 icon_128x128@2x.png"
    "256 icon_256x256.png"
    "512 icon_256x256@2x.png"
    "512 icon_512x512.png"
    "1024 icon_512x512@2x.png"
)

echo "Converting SVG to PNGs..."
for size_entry in "${SIZES[@]}"; do
    size=${size_entry%% *}
    filename=${size_entry#* }
    inkscape -w $size -h $size "$INPUT_SVG" -o "$ICONSET_DIR/$filename"
done

# Convert to ICNS
echo "Generating ICNS file..."
iconutil -c icns -o "$OUTPUT_ICNS" "$ICONSET_DIR"

# Clean up
rm -rf "$TEMP_DIR"

echo "Done! ICNS file saved to $OUTPUT_ICNS"
