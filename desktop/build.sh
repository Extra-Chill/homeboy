#!/usr/bin/env bash
set -euo pipefail

# Builds a Release archive, exports an .app,
# and produces a DMG installer in ./dist.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST_DIR="$ROOT_DIR/dist"
BUILD_DIR="$ROOT_DIR/.build"
ARCHIVE_PATH="$BUILD_DIR/Homeboy.xcarchive"
DMG_PATH="$DIST_DIR/Homeboy-macOS.dmg"
APP_NAME="Homeboy"

mkdir -p "$DIST_DIR" "$BUILD_DIR"

# Regenerate Xcode project from project.yml to ensure settings are current
echo "Regenerating Xcode project..."
if command -v xcodegen >/dev/null 2>&1; then
  xcodegen generate --spec "$ROOT_DIR/project.yml"
else
  echo "Warning: xcodegen not found. Using existing .xcodeproj (may be stale)." >&2
fi

if ! command -v xcodebuild >/dev/null 2>&1; then
  echo "xcodebuild not found. Install Xcode and CLI tools." >&2
  exit 1
fi

rm -rf "$ARCHIVE_PATH"

echo "Building Xcode archive..."
xcodebuild \
  -project "$ROOT_DIR/Homeboy.xcodeproj" \
  -scheme "Homeboy" \
  -configuration Release \
  -archivePath "$ARCHIVE_PATH" \
  archive

APP_PATH="$ARCHIVE_PATH/Products/Applications/$APP_NAME.app"
if [[ ! -d "$APP_PATH" ]]; then
  echo "Expected app not found in archive: $APP_PATH" >&2
  exit 1
fi

# Create DMG
echo "Creating DMG installer..."
rm -f "$DMG_PATH"

# Create temporary directory for DMG contents
DMG_TEMP="$BUILD_DIR/dmg-temp"
rm -rf "$DMG_TEMP"
mkdir -p "$DMG_TEMP"

# Copy app to temp directory
cp -R "$APP_PATH" "$DMG_TEMP/"

# Create Applications symlink
ln -s /Applications "$DMG_TEMP/Applications"

# Create DMG using hdiutil
hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$DMG_TEMP" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

# Cleanup temp
rm -rf "$DMG_TEMP"

echo "Built DMG: $DMG_PATH"
