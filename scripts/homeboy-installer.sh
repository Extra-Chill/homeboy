#!/bin/sh
set -eu

BIN_PATH="${HOMEBOY_INSTALL_PATH:-$(command -v homeboy 2>/dev/null || printf '%s' "$HOME/.local/bin/homeboy")}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "${OS}-${ARCH}" in
  linux-x86_64) ASSET="homeboy-x86_64-unknown-linux-gnu.tar.xz" ;;
  linux-aarch64|linux-arm64) ASSET="homeboy-aarch64-unknown-linux-gnu.tar.xz" ;;
  # darwin-x86_64 is intentionally unmapped: `x86_64-apple-darwin` was dropped
  # from `dist-workspace.toml`, so releases publish no Intel macOS tarball.
  darwin-aarch64|darwin-arm64) ASSET="homeboy-aarch64-apple-darwin.tar.xz" ;;
  darwin-x86_64) echo "Homeboy no longer publishes an Intel macOS (x86_64-apple-darwin) binary. Build from source: cargo install --git https://github.com/Extra-Chill/homeboy" >&2; exit 1 ;;
  *) echo "Unsupported platform: ${OS}-${ARCH}" >&2; exit 1 ;;
esac

TAG="${HOMEBOY_UPGRADE_RELEASE_TAG:-latest}"
TARGET_VERSION="${HOMEBOY_UPGRADE_RELEASE_VERSION:-}"
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

CANDIDATE="${ASSET%.tar.xz}/homeboy"
MEMBERS="$(env -u TAR_OPTIONS tar -tJf "$TMP_DIR/$ASSET" 2>/dev/null || env -u TAR_OPTIONS tar -tf "$TMP_DIR/$ASSET")" || {
  echo "Unable to list release archive" >&2
  exit 1
}
CANDIDATE_COUNT=0
while IFS= read -r member; do
  member="${member#./}"
  case "$member" in
    '' | /* | ../* | */../* | */.. | *//*)
      echo "Release archive contains an unsafe member path" >&2
      exit 1
      ;;
  esac
  case "$member" in
    homeboy | */homeboy)
      if [ "$member" = "$CANDIDATE" ]; then
        CANDIDATE_COUNT=$((CANDIDATE_COUNT + 1))
      else
        echo "Release archive contains an additional homeboy candidate" >&2
        exit 1
      fi
      ;;
  esac
done <<EOF
$MEMBERS
EOF
if [ "$CANDIDATE_COUNT" -ne 1 ]; then
  echo "Release archive must contain exactly one homeboy candidate" >&2
  exit 1
fi

CANDIDATE_DETAILS="$(env -u TAR_OPTIONS tar -tvJf "$TMP_DIR/$ASSET" "$CANDIDATE" 2>/dev/null || env -u TAR_OPTIONS tar -tvf "$TMP_DIR/$ASSET" "$CANDIDATE")" || {
  echo "Unable to inspect release candidate" >&2
  exit 1
}
case "$CANDIDATE_DETAILS" in
  -*) ;;
  *)
    echo "Release archive candidate must be a regular file" >&2
    exit 1
    ;;
esac

EXTRACTED_BIN="$TMP_DIR/homeboy"
(env -u TAR_OPTIONS tar -xJOf "$TMP_DIR/$ASSET" "$CANDIDATE" 2>/dev/null || env -u TAR_OPTIONS tar -xOf "$TMP_DIR/$ASSET" "$CANDIDATE") > "$EXTRACTED_BIN" || {
  echo "Unable to extract release candidate" >&2
  exit 1
}
chmod 0755 "$EXTRACTED_BIN"

# A checksum authenticates the archive; this exact version check binds the
# extracted executable to the selected target before it can repair ownership.
if [ -n "$TARGET_VERSION" ]; then
  CANDIDATE_VERSION="$($EXTRACTED_BIN --version 2>/dev/null | awk '{print $NF}' | sed 's/^v//; s/+.*$//')"
  if [ "$CANDIDATE_VERSION" != "$TARGET_VERSION" ]; then
    echo "Target-version bootstrap recovery refused: verified archive candidate reports ${CANDIDATE_VERSION:-unverifiable}, expected $TARGET_VERSION." >&2
    echo "Retry with matching release inputs: HOMEBOY_UPGRADE_RELEASE_TAG=v$TARGET_VERSION HOMEBOY_UPGRADE_RELEASE_VERSION=$TARGET_VERSION sh homeboy-installer.sh" >&2
    exit 1
  fi
fi

# The checksum- and version-verified staged candidate owns bounded recovery and
# admission; any failure exits before the installed controller is written.
LEGACY_IDENTITY="$("$BIN_PATH" self identity 2>/dev/null || "$BIN_PATH" --version 2>/dev/null || printf 'unavailable')"
# `set --` preserves the legacy identity as one argv element without an eval.
set -- self upgrade-admission --legacy-identity "$LEGACY_IDENTITY"
if [ -n "$TARGET_VERSION" ]; then
  set -- "$@" --target-version "$TARGET_VERSION"
fi
"$EXTRACTED_BIN" "$@"

if [ "${HOMEBOY_INSTALL_USE_SUDO:-false}" != true ] && { [ -w "$BIN_PATH" ] || [ -w "$BIN_DIR" ]; }; then
  install -m 0755 "$EXTRACTED_BIN" "$TMP_BIN"
  mv "$TMP_BIN" "$BIN_PATH"
else
  sudo install -m 0755 "$EXTRACTED_BIN" "$TMP_BIN"
  sudo mv "$TMP_BIN" "$BIN_PATH"
fi
