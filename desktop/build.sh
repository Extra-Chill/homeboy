#!/usr/bin/env bash
set -euo pipefail

# Builds a signed/unsigned Release archive, exports an .app,
# and produces a zip artifact in ./dist.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST_DIR="$ROOT_DIR/dist"
BUILD_DIR="$ROOT_DIR/.build"
ARCHIVE_PATH="$BUILD_DIR/Homeboy.xcarchive"
EXPORT_PATH="$BUILD_DIR/export"
ZIP_PATH="$DIST_DIR/Homeboy-macOS.zip"

mkdir -p "$DIST_DIR" "$BUILD_DIR"

if ! command -v xcodebuild >/dev/null 2>&1; then
  echo "xcodebuild not found. Install Xcode and CLI tools." >&2
  exit 1
fi

rm -rf "$ARCHIVE_PATH" "$EXPORT_PATH"

xcodebuild \
  -project "$ROOT_DIR/Homeboy.xcodeproj" \
  -scheme "Homeboy" \
  -configuration Release \
  -archivePath "$ARCHIVE_PATH" \
  archive

APP_PATH="$ARCHIVE_PATH/Products/Applications/Homeboy.app"
if [[ ! -d "$APP_PATH" ]]; then
  echo "Expected app not found in archive: $APP_PATH" >&2
  exit 1
fi

rm -f "$ZIP_PATH"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$ZIP_PATH"

echo "Built zip: $ZIP_PATH"
