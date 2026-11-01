#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-lwd-install.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
fail() { echo "FAIL: $1" >&2; exit 1; }

# No registry dependencies: the lockfile deliberately links an unbuilt sibling.
for extension in nodejs wordpress; do
    fixture="$TMP_DIR/$extension"
    consumer="$fixture/consumer"
    mkdir -p "$consumer" "$fixture/pkg"
    printf '%s\n' '{"name":"@scope/pkg","version":"1.0.0","main":"dist/index.js","files":["dist"],"scripts":{"build":"node build.js"}}' > "$fixture/pkg/package.json"
    printf '%s\n' 'const fs = require("fs"); fs.mkdirSync("dist", {recursive:true}); fs.writeFileSync("dist/index.js", "module.exports = 42;\n");' > "$fixture/pkg/build.js"
    printf '%s\n' '{"name":"consumer","version":"1.0.0","private":true,"dependencies":{"@scope/pkg":"file:../pkg"}}' > "$consumer/package.json"
    (cd "$consumer" && npm install --package-lock-only --no-audit --no-fund) > "$TMP_DIR/lock.out" 2>&1
    cp "$consumer/package.json" "$fixture/manifest.before"
    cp "$consumer/package-lock.json" "$fixture/lock.before"
    node -e 'const lock=require(process.argv[1]); if (!lock.packages["node_modules/@scope/pkg"].link) process.exit(1)' "$consumer/package-lock.json" \
        || fail "fixture lockfile does not link the sibling"
    runner="$ROOT_DIR/$extension/scripts/deps/deps-runner.sh"
    run_install() {
        (cd "$consumer" && HOMEBOY_COMPONENT_PATH="$consumer" HOMEBOY_SETTINGS_JSON="$1" bash "$runner" install)
    }
    for settings in '' '{}' '{"other":true}' '{"local_workspace_dependencies":[]}'; do
        HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS="$TMP_DIR/missing.sh" run_install "$settings" > "$TMP_DIR/install.out" 2>&1 \
            || { tail -50 "$TMP_DIR/install.out" >&2; fail "$extension no-op failed"; }
        [ ! -f "$consumer/node_modules/@scope/pkg/dist/index.js" ] || fail "no-op built the sibling"
    done
    settings='{"local_workspace_dependencies":[{"name":"@scope/pkg","path":"../pkg","package_dir":".","package_manager":"npm"}]}'
    run_install "$settings" > "$TMP_DIR/install.out" 2>&1 \
        || { tail -50 "$TMP_DIR/install.out" >&2; fail "$extension deps install failed"; }
    [ -f "$consumer/node_modules/@scope/pkg/dist/index.js" ] || fail "$extension deps install main missing"
    rm -rf "$fixture/pkg/dist" "$consumer/node_modules"
    # Exercise install-command too: core hydration must not bypass the hook.
    command_json="$(cd "$consumer" && HOMEBOY_COMPONENT_PATH="$consumer" bash "$runner" install-command)"
    (cd "$consumer" && HOMEBOY_COMPONENT_PATH="$consumer" HOMEBOY_SETTINGS_JSON="$settings" \
        node -e 'const cp=require("child_process"); const argv=JSON.parse(process.argv[1]).command; const r=cp.spawnSync(argv[0],argv.slice(1),{stdio:"inherit"}); process.exit(r.status ?? 1)' "$command_json") \
        > "$TMP_DIR/install.out" 2>&1 || { tail -50 "$TMP_DIR/install.out" >&2; fail "$extension install-command failed"; }
    [ -f "$consumer/node_modules/@scope/pkg/dist/index.js" ] || fail "$extension main file missing"
    [ ! -L "$consumer/node_modules/@scope/pkg" ] || fail "$extension left a symlink"
    (cd "$consumer" && node -e 'if(require("@scope/pkg") !== 42) process.exit(1)') || fail "package does not resolve"
    cmp "$fixture/manifest.before" "$consumer/package.json" || fail "manifest changed"
    cmp "$fixture/lock.before" "$consumer/package-lock.json" || fail "lockfile changed"

    for bad in '{"local_workspace_dependencies":[{"name":"@scope/pkg","path":"../missing"}]}' \
        '{"local_workspace_dependencies":[{"name":"@scope/pkg","path":"../pkg","package_dir":".","package_manager":"npm","build":"exit 23"}]}'; do
        if run_install "$bad" > "$TMP_DIR/error.out" 2>&1; then fail "$extension ignored failed override"; fi
        grep -Fq '[local-workspace-deps]' "$TMP_DIR/error.out" || fail "missing helper diagnostic"
    done
    if HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS="$TMP_DIR/missing.sh" run_install "$settings" > "$TMP_DIR/error.out" 2>&1; then
        fail "$extension ignored missing helper"
    fi
    grep -Fq 'helper not found' "$TMP_DIR/error.out" || fail "missing helper diagnostic"
    echo "ok: $extension installs built file: sibling; empty settings skip; declared failures are fatal"
done

echo 'local workspace deps install smoke passed.'
