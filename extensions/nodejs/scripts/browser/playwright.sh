#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SOURCE_DIR="${EXTENSION_PATH}/runtime/playwright"
RUNTIME_DIR="${HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/homeboy/nodejs-playwright}"
PACKAGE_DIR="${RUNTIME_DIR}/package"
BROWSER_CACHE_DIR="${RUNTIME_DIR}/browsers"
NODE_BIN="${HOMEBOY_NODEJS_PLAYWRIGHT_NODE:-node}"
NPM_BIN="${HOMEBOY_NODEJS_PLAYWRIGHT_NPM:-npm}"
STAT_BIN="${HOMEBOY_NODEJS_PLAYWRIGHT_STAT:-stat}"
ACTION="${1:-status}"

setup_command() { printf 'homeboy extension action nodejs browser.playwright.setup'; }
hash_sources() { ( cat "$SOURCE_DIR/package.json"; cat "$SOURCE_DIR/package-lock.json" ) | shasum -a 256 | awk '{print $1}'; }

require_node() {
    "$NODE_BIN" -e "const [major, minor] = process.versions.node.split('.').map(Number); process.exit(major > 20 || (major === 20 && minor >= 6) ? 0 : 1)" || {
        echo "Node.js >=20.6.0 is required for the Playwright utility (node:module register()). Install a compatible Node.js runtime." >&2
        exit 69
    }
}

assert_safe_path() {
    local path="$1"
    [ ! -L "$path" ] || { echo "Refusing symlinked Playwright runtime path: $path" >&2; return 1; }
    if [ -e "$path" ]; then
        local owner mode probe_owner probe_mode
        if probe_owner="$("$STAT_BIN" -c '%u' "$path" 2>/dev/null)" \
            && [[ "$probe_owner" =~ ^[0-9]+$ ]] \
            && probe_mode="$("$STAT_BIN" -c '%a' "$path" 2>/dev/null)" \
            && [[ "$probe_mode" =~ ^[0-7]+$ ]]; then
            owner="$probe_owner"
            mode="$probe_mode"
        elif probe_owner="$("$STAT_BIN" -f '%u' "$path" 2>/dev/null)" \
            && [[ "$probe_owner" =~ ^[0-9]+$ ]] \
            && probe_mode="$("$STAT_BIN" -f '%Lp' "$path" 2>/dev/null)" \
            && [[ "$probe_mode" =~ ^[0-7]+$ ]]; then
            owner="$probe_owner"
            mode="$probe_mode"
        else
            echo "Unable to read numeric owner and mode for runtime path: $path" >&2
            return 1
        fi
        [ "$owner" = "$(id -u)" ] || { echo "Refusing runtime path not owned by this user: $path" >&2; return 1; }
        [ $(( 8#$mode & 8#022 )) -eq 0 ] || { echo "Refusing group/world-writable runtime path: $path" >&2; return 1; }
    fi
}

assert_runtime_tree() {
    local target="$1" current="$RUNTIME_DIR" relative
    assert_safe_path "$current" || return
    relative="${target#"${RUNTIME_DIR}"/}"
    [ "$relative" = "$target" ] && { assert_safe_path "$target"; return; }
    local segment
    IFS=/ read -r -a segments <<< "$relative"
    for segment in "${segments[@]}"; do
        current="${current}/${segment}"
        [ ! -e "$current" ] || assert_safe_path "$current" || return
    done
}

package_state() {
    local expected="$1"
    [ -d "$PACKAGE_DIR" ] || { printf 'missing'; return; }
    if ! assert_runtime_tree "$PACKAGE_DIR" || ! assert_runtime_tree "$PACKAGE_DIR/node_modules" || ! assert_runtime_tree "$PACKAGE_DIR/node_modules/playwright"; then
        printf 'invalid'; return
    fi
    if "$NODE_BIN" - "$PACKAGE_DIR" "$expected" "$SOURCE_DIR/package.json" <<'NODE' >/dev/null 2>&1
const fs = require('node:fs'); const path = require('node:path');
const [dir, expected, source] = process.argv.slice(2);
try {
  for (const entry of ['package.json', 'package-lock.json', '.homeboy-runtime-sha256', 'node_modules/playwright/package.json', 'node_modules/playwright/cli.js']) {
    const file = path.join(dir, entry); if (fs.lstatSync(file).isSymbolicLink()) throw new Error('symlink');
  }
  if (fs.readFileSync(path.join(dir, '.homeboy-runtime-sha256'), 'utf8').trim() !== expected) throw new Error('hash');
  const wanted = JSON.parse(fs.readFileSync(source)).dependencies.playwright;
  if (JSON.parse(fs.readFileSync(path.join(dir, 'node_modules/playwright/package.json'))).version !== wanted) throw new Error('version');
} catch { process.exit(1); }
NODE
    then printf 'ready'; else printf 'invalid'; fi
}

browser_state() {
    [ "$1" = ready ] || { printf 'missing'; return; }
    if assert_runtime_tree "$BROWSER_CACHE_DIR" && PLAYWRIGHT_BROWSERS_PATH="$BROWSER_CACHE_DIR" "$NODE_BIN" - "$PACKAGE_DIR" <<'NODE' >/dev/null 2>&1
const { createRequire } = require('node:module'); const { lstatSync } = require('node:fs'); const { spawnSync } = require('node:child_process'); const path = require('node:path');
try { const p = createRequire(path.join(process.argv[2], 'package.json'))('playwright'); const bin = p.chromium.executablePath(); if (lstatSync(bin).isSymbolicLink()) throw 0; const result = spawnSync(bin, ['--version'], { encoding: 'utf8' }); process.exit(result.status === 0 ? 0 : 1); } catch { process.exit(1); }
NODE
    then printf 'ready'; else printf 'invalid'; fi
}

status() {
    require_node
    local hash package browser
    hash="$(hash_sources)"; package="$(package_state "$hash")"; browser="$(browser_state "$package")"
    if [ "${2:-}" = --json ]; then
        "$NODE_BIN" - "$package" "$browser" "$PACKAGE_DIR" "$BROWSER_CACHE_DIR" "$(setup_command)" <<'NODE'
const [packageState, chromiumState, runtimePackageDir, browserCacheDir, setupCommand] = process.argv.slice(2);
const reason = (state, kind) => state === 'ready' ? null : state === 'missing' ? `${kind} missing` : `${kind} invalid, corrupt, or version-mismatched`;
console.log(JSON.stringify({ package: { state: packageState, reason: reason(packageState, 'package') }, chromium: { state: chromiumState, reason: reason(chromiumState, 'Chromium') }, runtime_package_dir: runtimePackageDir, browser_cache_dir: browserCacheDir, setup_command: setupCommand }));
NODE
    else
        printf 'Playwright package: %s\nChromium: %s\n' "$package" "$browser"
        [ "$package" = ready ] && [ "$browser" = ready ] || printf 'Setup: %s\n' "$(setup_command)"
    fi
}

setup() {
    require_node; assert_safe_path "$RUNTIME_DIR"; mkdir -p "$RUNTIME_DIR"; chmod 700 "$RUNTIME_DIR"; assert_safe_path "$RUNTIME_DIR"
    mkdir -p "$BROWSER_CACHE_DIR"; chmod 700 "$BROWSER_CACHE_DIR"; assert_runtime_tree "$BROWSER_CACHE_DIR"
    local hash state tmp
    hash="$(hash_sources)"; state="$(package_state "$hash")"
    if [ "$state" != ready ]; then
        tmp="${RUNTIME_DIR}/.package.tmp.$$"; rm -rf "$tmp"; mkdir -m 700 "$tmp"
        cp "$SOURCE_DIR/package.json" "$SOURCE_DIR/package-lock.json" "$tmp/"; "$NPM_BIN" ci --prefix "$tmp" --ignore-scripts
        printf '%s\n' "$hash" > "$tmp/.homeboy-runtime-sha256"; chmod -R go-rwx "$tmp"
        local installed_package_dir="$PACKAGE_DIR"; PACKAGE_DIR="$tmp"
        [ "$(package_state "$hash")" = ready ] || { rm -rf "$tmp"; echo 'Installed Playwright runtime failed validation.' >&2; exit 1; }
        PACKAGE_DIR="$installed_package_dir"; rm -rf "${RUNTIME_DIR}/.package.previous"; [ ! -e "$PACKAGE_DIR" ] || mv "$PACKAGE_DIR" "${RUNTIME_DIR}/.package.previous"; mv "$tmp" "$PACKAGE_DIR"; rm -rf "${RUNTIME_DIR}/.package.previous"
    fi
    local package browser; package="$(package_state "$hash")"; browser="$(browser_state "$package")"
    if [ "$browser" != ready ]; then PLAYWRIGHT_BROWSERS_PATH="$BROWSER_CACHE_DIR" "$NODE_BIN" "$PACKAGE_DIR/node_modules/playwright/cli.js" install chromium; fi
    status status
}

execute() {
    require_node; local module="${1:-}"; shift || true; [ -n "$module" ] || { echo 'usage: playwright.sh execute <module> [args...]' >&2; exit 64; }
    local hash package browser; hash="$(hash_sources)"; package="$(package_state "$hash")"; browser="$(browser_state "$package")"
    [ "$package" = ready ] && [ "$browser" = ready ] || { echo "Playwright utility is not ready. Run: $(setup_command)" >&2; exit 69; }
    PLAYWRIGHT_BROWSERS_PATH="$BROWSER_CACHE_DIR" HOMEBOY_NODEJS_PLAYWRIGHT_RUNTIME_PACKAGE_DIR="$PACKAGE_DIR" "$NODE_BIN" --require "$SCRIPT_DIR/playwright-require.cjs" --import "$SCRIPT_DIR/register-playwright-loader.mjs" "$SCRIPT_DIR/execute-playwright-module.mjs" "$module" "$@"
}

action_execute() {
    local -a argv=()
    while IFS= read -r -d '' arg; do argv+=("$arg"); done < <("$NODE_BIN" -e '
const payload = JSON.parse(process.env.HOMEBOY_SETTINGS_JSON || "{}");
if (!Array.isArray(payload.selected) || payload.selected.length !== 1) throw new Error("selected must contain exactly one row");
const row = payload.selected[0];
if (!row || typeof row !== "object" || Array.isArray(row)) throw new Error("selected row must be an object");
if (typeof row.module !== "string" || row.module.length === 0) throw new Error("selected row module must be a non-empty string");
if (row.args !== undefined && (!Array.isArray(row.args) || !row.args.every((value) => typeof value === "string"))) throw new Error("selected row args must be an array of strings");
for (const value of [row.module, ...(row.args || [])]) process.stdout.write(value + "\0");
')
    [ "${#argv[@]}" -gt 0 ] || { echo 'selected row did not provide a module' >&2; exit 64; }
    execute "${argv[@]}"
}

case "$ACTION" in
 setup) setup ;; status) status "$@" ;; execute) shift; execute "$@" ;; action-execute) action_execute ;; *) echo "unknown Playwright utility action: $ACTION" >&2; exit 64 ;; esac
