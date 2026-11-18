#!/usr/bin/env bash
set -euo pipefail

# Regression smoke for homeboy#18302: declared local_workspace_dependencies are
# built, packed, and installed BEFORE any consumer lifecycle or build script
# runs, on every path that installs or builds a component's JS:
#   * WordPress and Node.js deps runners (`dependency.setup` for review/test):
#     `npm ci`/`npm install` must not run the consumer's `prepare` against an
#     unbuilt `file:` link.
#   * The WP Codebox PHPUnit adapter's host-side `wp_codebox_prepare_steps`.
# Components without a declaration keep running their lifecycle normally.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-lwd-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
fail() { echo "FAIL: $1" >&2; exit 1; }

# Fixture: an unbuilt sibling (no dist/ until its build runs) and a consumer
# whose `prepare` and `build` both require the BUILT sibling, mirroring a
# component with `"prepare": "npm run build"`.
make_fixture() {
    local fixture="$1" lockfile="$2"
    mkdir -p "$fixture/pkg" "$fixture/consumer"
    printf '%s\n' '{"name":"@scope/pkg","version":"1.0.0","main":"dist/index.js","exports":{".":"./dist/index.js"},"files":["dist"],"scripts":{"build":"node build.js"}}' > "$fixture/pkg/package.json"
    printf '%s\n' 'const fs = require("fs"); fs.mkdirSync("dist", {recursive:true}); fs.writeFileSync("dist/index.js", "module.exports = 42;\n");' > "$fixture/pkg/build.js"
    printf '%s\n' 'if (require("@scope/pkg") !== 42) process.exit(3); require("fs").writeFileSync(process.argv[2], "ok\n");' > "$fixture/consumer/needs-built.js"
    printf '%s\n' '{"name":"consumer","version":"1.0.0","private":true,"dependencies":{"@scope/pkg":"file:../pkg"},"scripts":{"prepare":"node needs-built.js prepared.txt","build":"node needs-built.js built.txt","fail-sentinel":"node -e \"process.exit(42)\""}}' > "$fixture/consumer/package.json"
    if [ "$lockfile" = lock ]; then
        (cd "$fixture/consumer" && npm install --package-lock-only --ignore-scripts --no-audit --no-fund) > "$TMP_DIR/lock.out" 2>&1 \
            || { cat "$TMP_DIR/lock.out" >&2; fail "fixture lockfile"; }
    fi
}

settings='{"local_workspace_dependencies":[{"name":"@scope/pkg","path":"../pkg","package_dir":".","package_manager":"npm"}]}'

# --- deps runners ---------------------------------------------------------------
for extension in wordpress nodejs; do
    for lockfile in lock nolock; do
        fixture="$TMP_DIR/$extension-$lockfile"
        make_fixture "$fixture" "$lockfile"
        consumer="$fixture/consumer"
        out="$TMP_DIR/install.out"
        (cd "$consumer" && HOMEBOY_COMPONENT_PATH="$consumer" HOMEBOY_SETTINGS_JSON="$settings" \
            bash "$ROOT_DIR/$extension/scripts/deps/deps-runner.sh" install) > "$out" 2>&1 \
            || { tail -40 "$out" >&2; fail "$extension ($lockfile) install ran consumer lifecycle before the override"; }
        [ -f "$consumer/prepared.txt" ] || { tail -40 "$out" >&2; fail "$extension ($lockfile) did not run the consumer prepare lifecycle"; }
        [ ! -L "$consumer/node_modules/@scope/pkg" ] || fail "$extension ($lockfile) left the unbuilt file: symlink"
        grep -Fq '[local-workspace-deps]' "$out" || fail "$extension ($lockfile) override did not run"
        echo "ok: $extension deps install ($lockfile) applies the override before consumer prepare"
    done

    # No declaration: lifecycle scripts still run exactly as npm runs them.
    plain="$TMP_DIR/$extension-plain"
    mkdir -p "$plain"
    printf '%s\n' '{"name":"plain","version":"1.0.0","private":true,"scripts":{"prepare":"node -e \"require(\\\"fs\\\").writeFileSync(\\\"prepared.txt\\\",\\\"ok\\\")\""}}' > "$plain/package.json"
    (cd "$plain" && HOMEBOY_COMPONENT_PATH="$plain" HOMEBOY_SETTINGS_JSON='{"local_workspace_dependencies":[]}' \
        HOMEBOY_RUNTIME_LOCAL_WORKSPACE_DEPS="$TMP_DIR/missing.sh" \
        bash "$ROOT_DIR/$extension/scripts/deps/deps-runner.sh" install) > "$TMP_DIR/plain.out" 2>&1 \
        || { tail -40 "$TMP_DIR/plain.out" >&2; fail "$extension install without overrides failed"; }
    [ -f "$plain/prepared.txt" ] || fail "$extension install without overrides skipped prepare"
    ! grep -Fq 'ignore-scripts' "$TMP_DIR/plain.out" || fail "$extension deferred lifecycle without overrides"
    echo "ok: $extension deps install without overrides keeps npm lifecycle behavior"
done

# --- WP Codebox prepare steps ---------------------------------------------------
# Dependencies are installed, but the file: link still points at the unbuilt
# sibling (as after a plain checkout + `npm ci --ignore-scripts`). The prepare
# step `npm run build` must see the built override. A trailing sentinel step
# fails on purpose so the adapter stops deterministically right after the
# prepare phase, without needing the WP Codebox runtime.
fixture="$TMP_DIR/codebox"
make_fixture "$fixture" lock
consumer="$fixture/consumer"
printf '%s\n' '<?php /* Plugin Name: Consumer */' > "$consumer/consumer.php"
(cd "$consumer" && npm ci --ignore-scripts --no-audit --no-fund) > "$TMP_DIR/ci.out" 2>&1 || fail "fixture npm ci"
[ ! -f "$fixture/pkg/dist/index.js" ] || fail "fixture sibling unexpectedly built"
run_adapter() {
    (cd "$consumer" && HOMEBOY_COMPONENT_PATH="$consumer" HOMEBOY_COMPONENT_ID=consumer HOMEBOY_SETTINGS_JSON="$1" \
        node "$ROOT_DIR/wordpress/scripts/test/wp-codebox-phpunit-adapter.mjs") > "$TMP_DIR/adapter.out" 2>&1 || true
}
prepare_settings="$(node -e '
    const s = JSON.parse(process.argv[1]);
    s.wp_codebox_prepare_steps = [
        { command: "npm", args: ["run", "build"] },
        { command: "npm", args: ["run", "fail-sentinel"] },
    ];
    process.stdout.write(JSON.stringify(s));
' "$settings")"
run_adapter "$prepare_settings"
grep -Fq 'wp_codebox_prepare_steps command failed (npm, exit 42)' "$TMP_DIR/adapter.out" \
    || { tail -40 "$TMP_DIR/adapter.out" >&2; fail "wp_codebox_prepare_steps build ran before the override"; }
[ -f "$consumer/built.txt" ] || { tail -40 "$TMP_DIR/adapter.out" >&2; fail "prepare step build output missing"; }
grep -Fq '[local-workspace-deps]' "$TMP_DIR/adapter.out" || fail "prepare-steps override did not run"
echo "ok: wp_codebox_prepare_steps run after the override is installed"

# A failing declared override stops before any prepare step runs.
rm -f "$consumer/built.txt"
bad_settings='{"local_workspace_dependencies":[{"name":"@scope/pkg","path":"../missing"}],"wp_codebox_prepare_steps":[{"command":"npm","args":["run","build"]}]}'
run_adapter "$bad_settings"
grep -Fq 'local_workspace_dependencies override failed before wp_codebox_prepare_steps' "$TMP_DIR/adapter.out" \
    || { tail -40 "$TMP_DIR/adapter.out" >&2; fail "failed override did not stop prepare steps"; }
[ ! -f "$consumer/built.txt" ] || fail "prepare step ran after a failed override"
echo "ok: a failed override stops wp_codebox_prepare_steps"

echo 'local workspace deps lifecycle smoke passed.'
