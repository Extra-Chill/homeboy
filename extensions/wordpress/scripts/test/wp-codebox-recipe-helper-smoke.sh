#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WP_CODEBOX_HELPER="${SCRIPT_DIR}/../lib/wp-codebox-paths.sh"

# shellcheck source=../lib/wp-codebox-paths.sh
source "$WP_CODEBOX_HELPER"

fixture="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-recipe-helper.XXXXXX")"
trap 'rm -rf "$fixture"' EXIT

fake_wp_codebox="${fixture}/wp-codebox.cjs"
stale_path="${fixture}/stale-bin"
valid_path="${fixture}/valid-bin"
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
if [ "${1:-}" = "--version" ]; then
    echo "fixture-wp-codebox 1.0.0"
    exit 0
fi
exit 0
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

resolved_bin=$(PATH="${stale_path}:${valid_path}:${PATH}" homeboy_wp_codebox_resolve_bin '{}')
if [ "$resolved_bin" != "${valid_path}/wp-codebox" ]; then
    echo "Expected resolver to skip stale wp-codebox wrapper and select working binary" >&2
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

echo "WP Codebox recipe helper smoke passed."
