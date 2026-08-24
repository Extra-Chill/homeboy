#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WP_CODEBOX_HELPER="${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"

# shellcheck source=../lib/wp-codebox-paths.sh
source "$WP_CODEBOX_HELPER"

fixture="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-recipe-helper.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT

fake_wp_codebox="${fixture}/wp-codebox.cjs"
managed_source="${fixture}/managed/source"
managed_wp_codebox="${managed_source}/packages/cli/dist/index.js"
stale_path="${fixture}/stale-bin"
valid_path="${fixture}/valid-bin"
global_node_root="${fixture}/global-node-modules"
js_bin="${fixture}/wp-codebox-js-entry.mjs"
recipe_file="${fixture}/recipe.json"
artifacts_dir="${fixture}/artifacts"
output_file="${fixture}/recipe-output.json"
stderr_file="${fixture}/recipe-stderr.txt"
capture_file="${fixture}/capture.txt"
mkdir -p "$artifacts_dir"

cat > "$fake_wp_codebox" <<'NODE'
#!/usr/bin/env node
const fs = require('node:fs');

if (process.argv.includes('--version')) {
  process.stdout.write('0.21.0');
  process.exit(0);
}
if (process.argv.slice(-3).join(' ') === 'runtime descriptor --json') {
  process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } }));
  process.exit(0);
}
if (process.argv.includes('commands')) {
  process.exit(0);
}
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_CAPTURE, process.argv.slice(2).join('\n') + '\n');
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ stdout: 'helper stdout\n', stderr: 'helper stderr\n' }],
  artifacts: { directory: process.argv[process.argv.indexOf('--artifacts') + 1] },
}, null, 2));
NODE
chmod +x "$fake_wp_codebox"

mkdir -p "$stale_path" "$valid_path"
cat > "${stale_path}/wp-codebox" <<'SH'
#!/usr/bin/env bash
exec node "/definitely/missing/wp-codebox.js" "$@"
SH
chmod +x "${stale_path}/wp-codebox"
cat > "${valid_path}/wp-codebox" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = "commands" ]; then
    echo "fixture-wp-codebox commands"
    exit 0
fi
exit 1
SH
chmod +x "${valid_path}/wp-codebox"

cat > "$js_bin" <<'NODE'
#!/usr/bin/env node
process.exit(2);
NODE

if ! homeboy_wp_codebox_bin_is_runnable "$js_bin"; then
    echo "Expected resolver validation to accept readable JS CLI entrypoints by existence" >&2
    exit 1
fi

resolved_bin=$(HOMEBOY_WP_CODEBOX_INSTALL_DIR="${fixture}/empty-managed" PATH="${stale_path}:${valid_path}:${PATH}" homeboy_wp_codebox_resolve_bin '{}')
if [ "$resolved_bin" != "${valid_path}/wp-codebox" ]; then
    echo "Expected resolver to skip stale wp-codebox wrapper and select working binary" >&2
    echo "Resolved: ${resolved_bin}" >&2
    exit 1
fi

resolved_bin=$(HOMEBOY_WP_CODEBOX_INSTALL_DIR="${fixture}/empty-managed" HOMEBOY_SETTINGS_JSON="{\"wpCodeboxBin\":\"${fake_wp_codebox}\"}" homeboy_wp_codebox_resolve_bin)
if [ "$resolved_bin" != "$fake_wp_codebox" ]; then
    echo "Expected camel-case settings pin to resolve the WP Codebox binary" >&2
    exit 1
fi

if HOMEBOY_WP_CODEBOX_INSTALL_DIR="${fixture}/empty-managed" HOMEBOY_SETTINGS_JSON="{\"wpCodeboxBin\":\"${fixture}/missing-wp-codebox\"}" homeboy_wp_codebox_resolve_bin >/dev/null 2>&1; then
    echo "Expected missing camel-case settings pin to fail closed" >&2
    exit 1
fi

global_cli="${global_node_root}/wp-codebox-workspace/packages/cli/dist/index.js"
mkdir -p "$(dirname "$global_cli")"
printf '%s\n' 'process.exit(0);' > "$global_cli"
resolved_bin=$(HOMEBOY_WP_CODEBOX_INSTALL_DIR="${fixture}/empty-managed" PATH="${stale_path}" HOMEBOY_GLOBAL_NODE_MODULE_ROOT="$global_node_root" homeboy_wp_codebox_resolve_bin '{}')
if [ "$resolved_bin" != "$global_cli" ]; then
    echo "Expected resolver to select global npm WP Codebox CLI when PATH wrappers are stale" >&2
    echo "Resolved: ${resolved_bin}" >&2
    exit 1
fi

printf '{"schema":"wp-codebox/workspace-recipe/v1","workflow":{"steps":[]}}\n' > "$recipe_file"

FAKE_WP_CODEBOX_CAPTURE="$capture_file" \
    homeboy_wp_codebox_run_recipe "$recipe_file" "$artifacts_dir" "$output_file" "$stderr_file" "$fake_wp_codebox"

if ! grep -Fxq 'recipe-run' "$capture_file"; then
    echo "Expected helper to invoke wp-codebox recipe-run" >&2
    exit 1
fi

if [ "$(homeboy_wp_codebox_recipe_last_stdout "$output_file")" != "helper stdout" ]; then
    echo "Expected helper to extract last execution stdout" >&2
    exit 1
fi

if [ "$(homeboy_wp_codebox_recipe_last_stderr "$output_file")" != "helper stderr" ]; then
    echo "Expected helper to extract last execution stderr" >&2
    exit 1
fi

if ! homeboy_wp_codebox_recipe_succeeded "$output_file"; then
    echo "Expected helper to report successful recipe result" >&2
    exit 1
fi

if [ "$(homeboy_wp_codebox_recipe_artifact_directory "$output_file")" != "$artifacts_dir" ]; then
    echo "Expected helper to extract artifact directory" >&2
    exit 1
fi

mkdir -p "$(dirname "$managed_wp_codebox")"
cp "$fake_wp_codebox" "$managed_wp_codebox"
git init -q "$managed_source"
git -C "$managed_source" add .
git -C "$managed_source" -c user.name=Fixture -c user.email=fixture@example.test commit -qm fixture
managed_sha="$(git -C "$managed_source" rev-parse HEAD)"
managed_sha256="$(node -e 'const crypto=require("node:crypto"); const fs=require("node:fs"); process.stdout.write(crypto.createHash("sha256").update(fs.readFileSync(process.argv[1])).digest("hex"));' "$managed_wp_codebox")"
printf '%s\n' "{\"schema\":\"homeboy/wp-codebox-managed-runtime-identity/v1\",\"source_sha\":\"${managed_sha}\",\"cli_sha256\":\"${managed_sha256}\",\"required_capabilities\":[\"wp-codebox/browser-contained-site-open/v1\"]}" > "${managed_source}/.homeboy-runtime-identity.json"
printf '%s\n' '// tampered after setup' >> "$managed_wp_codebox"
rm -f "$capture_file"
if HOMEBOY_WP_CODEBOX_INSTALL_DIR="${fixture}/managed" FAKE_WP_CODEBOX_CAPTURE="$capture_file" homeboy_wp_codebox_run_recipe "$recipe_file" "$artifacts_dir" "$output_file" "$stderr_file" "$managed_wp_codebox" >/dev/null 2>&1; then
    echo "Expected a tampered managed recipe binary to fail preflight" >&2
    exit 1
fi
if [ -e "$capture_file" ]; then
    echo "Expected managed runtime preflight to reject the recipe before execution" >&2
    exit 1
fi

echo "WP Codebox recipe helper smoke passed."
