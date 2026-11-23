#!/usr/bin/env bash
set -euo pipefail

# The npm install bridge scripts/lib/local-workspace-install.sh is a shared
# asset (extensions/homeboy-extension-root.json) installed next to the
# extensions. When an older install source replaced the shared assets without
# it, the WordPress and Node.js deps runners died on a bare
# "No such file or directory" from `source`, and the WP Codebox prepare-step
# bridge failed the same way. Each must instead fail before installing, naming
# the expected shared path and the reinstall remedy.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-shared-helper-missing.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
fail() { echo "FAIL: $1" >&2; exit 1; }

# Installed layout: <extensions>/{wordpress,nodejs} plus the shared assets
# (scripts/lib, dependency-adapters) from an older source, i.e. present but
# without local-workspace-install.sh.
installed="$TMP_DIR/extensions"
mkdir -p "$installed/wordpress/scripts" "$installed/nodejs" "$installed/scripts"
cp -R "$ROOT_DIR/wordpress/scripts/deps" "$ROOT_DIR/wordpress/scripts/lib" "$installed/wordpress/scripts/"
cp -R "$ROOT_DIR/nodejs/scripts" "$ROOT_DIR/nodejs/dependency-adapters" "$installed/nodejs/"
cp -R "$ROOT_DIR/scripts/lib" "$installed/scripts/"
cp -R "$ROOT_DIR/dependency-adapters" "$installed/"
rm -f "$installed/scripts/lib/local-workspace-install.sh"
# Node reports the realpath (macOS /var -> /private/var); match the stable tail.
expected_path="/extensions/scripts/lib/local-workspace-install.sh"

consumer="$TMP_DIR/consumer"
mkdir -p "$consumer"
printf '%s\n' '{"name":"consumer","version":"1.0.0","private":true}' > "$consumer/package.json"

assert_clear_failure() {
    local label="$1" out="$2" extension_id="$3"
    grep -Eq "Homeboy shared extension helper is missing: /.*${expected_path}" "$out" \
        || { cat "$out" >&2; fail "${label}: did not name the missing shared helper path"; }
    grep -Fq "homeboy extension install https://github.com/Extra-Chill/homeboy --id ${extension_id}" "$out" \
        || { cat "$out" >&2; fail "${label}: did not name the reinstall remedy"; }
    if grep -Fq "No such file or directory" "$out"; then
        cat "$out" >&2
        fail "${label}: still surfaced the bare source error"
    fi
}

for extension in wordpress nodejs; do
    out="$TMP_DIR/${extension}.out"
    if (cd "$consumer" && HOMEBOY_COMPONENT_PATH="$consumer" HOMEBOY_SETTINGS_JSON='{}' \
        bash "$installed/$extension/scripts/deps/deps-runner.sh" install) > "$out" 2>&1; then
        cat "$out" >&2
        fail "$extension deps install succeeded without the shared helper"
    fi
    assert_clear_failure "$extension deps install" "$out" "$extension"
    [ ! -d "$consumer/node_modules" ] || fail "$extension ran the package manager before failing"
    echo "ok: $extension deps install fails clearly when the shared install helper is missing"
done

out="$TMP_DIR/prepare.out"
if HOMEBOY_PREPARE_MODULE="$installed/wordpress/scripts/lib/local-workspace-prepare.mjs" CONSUMER="$consumer" node --input-type=module -e '
    const { applyLocalWorkspaceDependenciesBeforePrepare } = await import(process.env.HOMEBOY_PREPARE_MODULE);
    applyLocalWorkspaceDependenciesBeforePrepare({
        settings: { local_workspace_dependencies: [{ name: "@scope/pkg", path: "../pkg" }] },
        steps: [{ command: "npm", args: ["run", "build"] }],
        consumerDirectory: process.env.CONSUMER,
    });
' > "$out" 2>&1; then
    cat "$out" >&2
    fail "wp_codebox_prepare_steps bridge succeeded without the shared helper"
fi
assert_clear_failure "wp_codebox_prepare_steps bridge" "$out" wordpress
echo "ok: wp_codebox_prepare_steps bridge fails clearly when the shared install helper is missing"

echo 'shared install helper missing smoke passed.'
