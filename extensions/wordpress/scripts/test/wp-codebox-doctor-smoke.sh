#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOCTOR="${SCRIPT_DIR}/../doctor/wp-codebox-doctor.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

FAKE_BIN="${TMPDIR}/wp-codebox"
CALLS="${TMPDIR}/calls.log"
ARCHIVE_ROOT="${TMPDIR}/archives"
mkdir -p "$ARCHIVE_ROOT"

cat > "$FAKE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$WP_CODEBOX_CALLS"
case "$1" in
    commands)
        printf 'doctor\ncleanup\n'
        ;;
    doctor)
        printf 'WP Codebox doctor: warning\n'
        ;;
    cleanup)
        printf 'WP Codebox cleanup: ok\n'
        ;;
    *)
        printf 'unexpected mode: %s\n' "$1" >&2
        exit 64
        ;;
esac
SH
chmod +x "$FAKE_BIN"

output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/empty-install" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    bash "$DOCTOR" doctor --fix --stale-after-seconds 1 --archive-root "$ARCHIVE_ROOT" --json 2>&1)

if [[ "$output" != *"WP Codebox doctor: warning"* ]]; then
    echo "Expected delegated doctor output" >&2
    echo "$output" >&2
    exit 1
fi

cleanup_output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/empty-install" \
    HOMEBOY_WP_CODEBOX_BIN="$FAKE_BIN" \
    bash "$DOCTOR" cleanup --stale-after-seconds 0 --archive-root "$ARCHIVE_ROOT" 2>&1)

if [[ "$cleanup_output" != *"WP Codebox cleanup: ok"* ]]; then
    echo "Expected delegated cleanup output" >&2
    echo "$cleanup_output" >&2
    exit 1
fi

if ! grep -Fq "doctor --fix --stale-after-seconds 1 --archive-root $ARCHIVE_ROOT --json" "$CALLS"; then
    echo "Expected doctor args to be passed through" >&2
    cat "$CALLS" >&2
    exit 1
fi

if ! grep -Fq "cleanup --stale-after-seconds 0 --archive-root $ARCHIVE_ROOT" "$CALLS"; then
    echo "Expected cleanup args to be passed through" >&2
    cat "$CALLS" >&2
    exit 1
fi

SETTINGS_BIN="${TMPDIR}/settings-wp-codebox"
cat > "$SETTINGS_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$WP_CODEBOX_CALLS"
case "$1" in
    commands)
        printf 'doctor\ncleanup\n'
        ;;
    *)
        printf 'settings wp-codebox %s\n' "$1"
        ;;
esac
SH
chmod +x "$SETTINGS_BIN"

settings_output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/empty-install" \
    HOMEBOY_SETTINGS_JSON="{\"wp_codebox_bin\":\"$SETTINGS_BIN\"}" \
    bash "$DOCTOR" --json 2>&1)

if [[ "$settings_output" != *"settings wp-codebox doctor"* ]]; then
    echo "Expected settings wp_codebox_bin to be used" >&2
    echo "$settings_output" >&2
    exit 1
fi

MANAGED_HOME="${TMPDIR}/managed-home"
MANAGED_BIN="${MANAGED_HOME}/.cache/homeboy/wp-codebox/source/packages/cli/dist/index.js"
STALE_BIN="${TMPDIR}/stale-wp-codebox"
RUNTIME_BIN="${TMPDIR}/runtime-wp-codebox"
mkdir -p "$(dirname "$MANAGED_BIN")"

cat > "$MANAGED_BIN" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs');
fs.appendFileSync(process.env.WP_CODEBOX_CALLS, `${process.argv.slice(2).join(' ')}\n`);
switch (process.argv[2]) {
  case 'commands': process.exit(0);
  case 'doctor': console.log('managed wp-codebox doctor'); break;
  default: process.exit(64);
}
NODE
chmod +x "$MANAGED_BIN"

cat > "$STALE_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'stale wp-codebox should not run\n' >&2
exit 65
SH
chmod +x "$STALE_BIN"

set +e
managed_output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOME="$MANAGED_HOME" \
    HOMEBOY_WP_CODEBOX_BIN="$STALE_BIN" \
    HOMEBOY_SETTINGS_JSON="{\"wp_codebox_bin\":\"$STALE_BIN\"}" \
    bash "$DOCTOR" --json 2>&1)
managed_status=$?
set -e

if [ "$managed_status" -eq 0 ] || [[ "$managed_output" != *"configured WP Codebox binary is unavailable"* ]]; then
    echo "Expected stale explicit WP Codebox config to fail without falling back to managed cache" >&2
    echo "$managed_output" >&2
    exit 1
fi

cat > "$RUNTIME_BIN" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$WP_CODEBOX_CALLS"
case "$1" in
    commands) exit 0 ;;
    doctor) printf 'runtime wp-codebox doctor\n' ;;
    *) exit 64 ;;
esac
SH
chmod +x "$RUNTIME_BIN"

runtime_output=$(WP_CODEBOX_CALLS="$CALLS" \
    HOME="$MANAGED_HOME" \
    HOMEBOY_SETTINGS_JSON="{\"runtime_bin\":\"$RUNTIME_BIN\"}" \
    bash "$DOCTOR" --json 2>&1)

if [[ "$runtime_output" != *"runtime wp-codebox doctor"* ]]; then
    echo "Expected generic runtime_bin to outrank managed WP Codebox cache" >&2
    echo "$runtime_output" >&2
    exit 1
fi

MINIMAL_BIN="${TMPDIR}/minimal-bin"
mkdir -p "$MINIMAL_BIN"
ln -s "$(command -v dirname)" "${MINIMAL_BIN}/dirname"

set +e
missing_output=$(HOME="${TMPDIR}/missing-home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/missing-install" \
    HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/missing-wp-codebox" \
    HOMEBOY_SETTINGS_JSON='{}' \
    PATH="$MINIMAL_BIN" \
    /bin/bash "$DOCTOR" doctor 2>&1)
missing_status=$?
set -e

if [ "$missing_status" -eq 0 ]; then
    echo "Expected doctor to fail when WP Codebox is unavailable" >&2
    echo "$missing_output" >&2
    exit 1
fi

if [[ "$missing_output" != *"configured WP Codebox binary is unavailable"* ]]; then
    echo "Expected fail-closed configured WP Codebox guidance" >&2
    echo "$missing_output" >&2
    exit 1
fi

echo "WP Codebox doctor wrapper smoke passed"
