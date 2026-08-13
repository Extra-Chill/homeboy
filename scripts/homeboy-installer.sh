#!/bin/sh
set -eu

BIN_PATH="${HOMEBOY_INSTALL_PATH:-$(command -v homeboy 2>/dev/null || printf '%s' "$HOME/.local/bin/homeboy")}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "${OS}-${ARCH}" in
  linux-x86_64) ASSET="homeboy-x86_64-unknown-linux-gnu.tar.xz" ;;
  linux-aarch64|linux-arm64) ASSET="homeboy-aarch64-unknown-linux-gnu.tar.xz" ;;
  darwin-x86_64) ASSET="homeboy-x86_64-apple-darwin.tar.xz" ;;
  darwin-aarch64|darwin-arm64) ASSET="homeboy-aarch64-apple-darwin.tar.xz" ;;
  *) echo "Unsupported platform: ${OS}-${ARCH}" >&2; exit 1 ;;
esac

TAG="${HOMEBOY_UPGRADE_RELEASE_TAG:-latest}"
if [ "$TAG" = latest ]; then
  BASE_URL="https://github.com/Extra-Chill/homeboy/releases/latest/download"
else
  BASE_URL="https://github.com/Extra-Chill/homeboy/releases/download/${TAG}"
fi

TMP_DIR="$(mktemp -d)"
BIN_DIR="$(dirname "$BIN_PATH")"
mkdir -p "$BIN_DIR"
TMP_BIN="$BIN_DIR/.homeboy-upgrade.$$"
cleanup() { rm -f "$TMP_BIN"; rm -rf "$TMP_DIR"; }
trap cleanup EXIT

curl -fsSL "${BASE_URL}/${ASSET}" -o "${TMP_DIR}/${ASSET}"
curl -fsSL "${BASE_URL}/${ASSET}.sha256" -o "${TMP_DIR}/${ASSET}.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$TMP_DIR" && sha256sum -c "${ASSET}.sha256")
else
  expected="$(cut -d' ' -f1 "${TMP_DIR}/${ASSET}.sha256")"
  actual="$(shasum -a 256 "${TMP_DIR}/${ASSET}" | cut -d' ' -f1)"
  [ "$expected" = "$actual" ]
fi

(cd "$TMP_DIR" && tar -xJf "$ASSET" 2>/dev/null || tar -xf "$ASSET")
if [ ! -f "$TMP_DIR/homeboy" ]; then
  echo "Expected extracted binary named homeboy" >&2
  exit 1
fi
chmod 0755 "$TMP_DIR/homeboy"

# The staged candidate owns admission; any failure exits before the installed
# controller is written. Its report binds the candidate decision to legacy identity.
LEGACY_IDENTITY="$("$BIN_PATH" self identity 2>/dev/null || "$BIN_PATH" --version 2>/dev/null || printf 'unavailable')"
"$TMP_DIR/homeboy" self upgrade-admission --legacy-identity "$LEGACY_IDENTITY"

if [ "${HOMEBOY_INSTALL_USE_SUDO:-false}" != true ] && { [ -w "$BIN_PATH" ] || [ -w "$BIN_DIR" ]; }; then
  install -m 0755 "$TMP_DIR/homeboy" "$TMP_BIN"
  mv "$TMP_BIN" "$BIN_PATH"
else
  sudo install -m 0755 "$TMP_DIR/homeboy" "$TMP_BIN"
  sudo mv "$TMP_BIN" "$BIN_PATH"
fi
