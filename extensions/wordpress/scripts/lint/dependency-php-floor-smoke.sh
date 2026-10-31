#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/component" "$TMP/data-machine" "$TMP/extension"
printf '%s\n' '<?php /* Plugin Name: Consumer */' > "$TMP/component/main.php"
printf '%s\n' 'homeboy_resolve_validation_dependency_paths() { printf "%s\n" "$TEST_DEPENDENCY"; }' > "$TMP/dependencies.sh"
printf '%s\n' 'homeboy_resolve_context() { EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"; PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"; }' > "$TMP/context.sh"
: > "$TMP/sidecar.sh"
running="$(php -r 'echo PHP_MAJOR_VERSION, ".", PHP_MINOR_VERSION;')"
higher="$(php -r 'echo PHP_MAJOR_VERSION + 1, ".0";')"
run_runner() {
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$TMP/context.sh" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$TMP/sidecar.sh" \
    HOMEBOY_EXTENSION_PATH="$TMP/extension" \
    HOMEBOY_COMPONENT_PATH="$TMP/component" \
    HOMEBOY_COMPONENT_ID=floor-smoke \
    HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$TMP/dependencies.sh" \
    TEST_DEPENDENCY="$TMP/data-machine" \
    bash "$SCRIPT_DIR/phpstan-runner.sh" > "$TMP/output" 2>&1
}
# Composer-only dependency: never load its unsupported declarations.
printf '{"require":{"php":">=%s"}}\n' "$higher" > "$TMP/data-machine/composer.json"
printf '%s\n' '<?php this is deliberately invalid PHP;' > "$TMP/data-machine/broken.php"
if run_runner; then
    echo 'FAIL: newer dependency floor should fail' >&2
    exit 1
fi
for expected in 'data-machine' "requires PHP >= $higher" "running PHP $running" 'extensions.wordpress.settings.wordpress_runtime_php_version'; do
    grep -F "$expected" "$TMP/output" >/dev/null
done
! grep -E 'Fatal error|infrastructure failure' "$TMP/output"
# Equal and lower floors must reach the normal missing-PHPStan pass-through.
for floor in "$running" '7.4'; do
    printf '{"require":{"php":">=%s"}}\n' "$floor" > "$TMP/data-machine/composer.json"
    run_runner
    grep -F 'PHPStan not found' "$TMP/output" >/dev/null
done
# Header takes precedence over Composer, including themes.
printf '/*\nTheme Name: Dependency\nRequires PHP: %s\n*/\n' "$higher" > "$TMP/data-machine/style.css"
if run_runner; then exit 1; fi
grep -F "requires PHP >= $higher" "$TMP/output" >/dev/null
rm "$TMP/data-machine/style.css"
# Detection raises the component floor only for present local checkouts.
printf '<?php\n/**\n * Plugin Name: Consumer\n * Requires PHP: 8.1\n */\n' > "$TMP/component/main.php"
printf '{"require":{"php":">=8.2"}}\n' > "$TMP/data-machine/composer.json"
printf '%s\n' '{"extensions":{"wordpress":{"settings":{"validation_dependencies":["../data-machine","missing-slug"]}}}}' > "$TMP/component/homeboy.json"
actual="$(cd "$TMP/component" && bash "$EXTENSION_DIR/scripts/env/detect.sh")"
python3 - "$actual" <<'PY'
import json, sys
assert json.loads(sys.argv[1])['runtimes']['php']['version'] == '8.2'
PY
python3 - "$TMP/component/homeboy.json" <<'PY'
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
data = json.loads(path.read_text())
data['extensions']['wordpress']['settings']['wordpress_runtime_php_version'] = '8.1'
path.write_text(json.dumps(data))
PY
actual="$(cd "$TMP/component" && bash "$EXTENSION_DIR/scripts/env/detect.sh")"
python3 - "$actual" "$SCRIPT_DIR/../lib" <<'PY'
import json, sys
assert json.loads(sys.argv[1])['runtimes']['php']['version'] == '8.1'
sys.path.insert(0, sys.argv[2])
from php_floor import composer_floor
for constraint, expected in [('>=8.2', '8.2'), ('^8.2', '8.2'), ('~8.2.0', '8.2'), ('>=8.2 <9', '8.2'), ('^8.1 || ^8.2', '8.1'), ('*', None), ('<8.3', None)]:
    assert composer_floor(constraint) == expected, constraint
PY
echo 'dependency PHP floor smoke passed'
