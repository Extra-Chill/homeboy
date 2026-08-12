#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT_DIR/scripts/bootstrap-standard-extensions.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -f "$SCRIPT" ]; then
    echo "Missing bootstrap script: $SCRIPT" >&2
    exit 1
fi

FAKE_HOMEBOY="$TMP_DIR/homeboy"
LOG_FILE="$TMP_DIR/homeboy.log"
cat > "$FAKE_HOMEBOY" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$HOMEBOY_FAKE_LOG"
case "$1 $2" in
    "extension install") exit 0 ;;
    "extension show") exit 0 ;;
    "extension list") printf 'nodejs\nrust\n' ; exit 0 ;;
    *) cat ;;
esac
FAKE
chmod +x "$FAKE_HOMEBOY"

OUTPUT="$(HOMEBOY_CONTROLLER_BIN="$FAKE_HOMEBOY" HOMEBOY_FAKE_LOG="$LOG_FILE" "$SCRIPT" --target local --extensions "nodejs rust" --repo "https://example.com/extensions.git" --homeboy /opt/homeboy/bin/homeboy --dry-run)"

case "$OUTPUT" in
    *'INSTALL_ARGS=(extension install "$REPO" --id "$EXTENSION_ID")'*'"$HOMEBOY_BIN" "${INSTALL_ARGS[@]}"'*) ;;
    *)
        echo "Dry-run output did not include install command" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac

case "$OUTPUT" in
    *'if [ "${REPLACE_EXISTING:-0}" = "1" ]; then'*'INSTALL_ARGS+=(--replace)'*) ;;
    *)
        echo "Dry-run output did not include conditional replace mode" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac

case "$OUTPUT" in
    *'"$HOMEBOY_BIN" extension show "$EXTENSION_ID" >/dev/null'*) ;;
    *)
        echo "Dry-run output did not include verify command" >&2
        echo "$OUTPUT" >&2
        exit 1
        ;;
esac

HELP_OUTPUT="$($SCRIPT --help)"
case "$HELP_OUTPUT" in
    *"--target <runner-id>"*"--replace-existing"*"--dry-run"*) ;;
    *)
        echo "Help output is missing expected options" >&2
        echo "$HELP_OUTPUT" >&2
        exit 1
        ;;
esac

: > "$LOG_FILE"
HOMEBOY_CONTROLLER_BIN="$FAKE_HOMEBOY" HOMEBOY_FAKE_LOG="$LOG_FILE" "$SCRIPT" --target homeboy-lab --extensions "nodejs rust" --repo "https://example.com/extensions.git" --homeboy /opt/homeboy/bin/homeboy --dry-run >/dev/null
TARGET_CALL="$(<"$LOG_FILE")"
case "$TARGET_CALL" in
    *"runner exec homeboy-lab --script-file - --raw --env HOMEBOY_BIN=/opt/homeboy/bin/homeboy --env REPO=https://example.com/extensions.git --env EXTENSIONS=nodejs rust --env REPLACE_EXISTING=0 --dry-run"*) ;;
    *)
        echo "Targeted dry-run did not preserve runner and environment arguments" >&2
        echo "$TARGET_CALL" >&2
        exit 1
        ;;
esac
case "$TARGET_CALL" in
    *"--ssh"*)
        echo "Targeted dry-run unexpectedly requested diagnostic SSH" >&2
        echo "$TARGET_CALL" >&2
        exit 1
        ;;
esac

: > "$LOG_FILE"
HOMEBOY_FAKE_LOG="$LOG_FILE" "$SCRIPT" --extensions "nodejs rust" --repo "https://example.com/extensions.git" --homeboy "$FAKE_HOMEBOY" >/dev/null

EXPECTED_LOG="$TMP_DIR/expected.log"
cat > "$EXPECTED_LOG" <<EOF
extension install https://example.com/extensions.git --id nodejs
extension show nodejs
extension install https://example.com/extensions.git --id rust
extension show rust
extension list
EOF

if ! cmp -s "$EXPECTED_LOG" "$LOG_FILE"; then
    echo "Local bootstrap did not call Homeboy as expected" >&2
    echo "Expected:" >&2
    sed 's/^/  /' "$EXPECTED_LOG" >&2
    echo "Actual:" >&2
    sed 's/^/  /' "$LOG_FILE" >&2
    exit 1
fi

: > "$LOG_FILE"
HOMEBOY_FAKE_LOG="$LOG_FILE" "$SCRIPT" --extensions "wordpress" --repo "https://example.com/extensions.git" --homeboy "$FAKE_HOMEBOY" --replace-existing >/dev/null

cat > "$EXPECTED_LOG" <<EOF
extension install https://example.com/extensions.git --id wordpress --replace
extension show wordpress
extension list
EOF

if ! cmp -s "$EXPECTED_LOG" "$LOG_FILE"; then
    echo "Repair bootstrap did not call Homeboy with --replace" >&2
    echo "Expected:" >&2
    sed 's/^/  /' "$EXPECTED_LOG" >&2
    echo "Actual:" >&2
    sed 's/^/  /' "$LOG_FILE" >&2
    exit 1
fi

echo "bootstrap standard extensions smoke passed"
