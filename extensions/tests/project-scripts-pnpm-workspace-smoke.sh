#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT_SCRIPTS="${ROOT_DIR}/scripts/lib/project-scripts.sh"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

WORKSPACE_PROJECT="$TMP_DIR/workspace-project"
mkdir -p "$WORKSPACE_PROJECT/packages/plugin/subdir"
cat > "$WORKSPACE_PROJECT/package.json" <<'EOF'
{"name":"workspace-root"}
EOF
cat > "$WORKSPACE_PROJECT/packages/plugin/package.json" <<'EOF'
{"name":"workspace-plugin","scripts":{"test":"node plugin.js"}}
EOF
touch "$WORKSPACE_PROJECT/pnpm-lock.yaml" "$WORKSPACE_PROJECT/pnpm-workspace.yaml"

bash -c '
    source "$1"
    homeboy_project_init --ecosystem node --path "$2/packages/plugin/subdir"
    [ "$HOMEBOY_PROJECT_ROOT" = "$2/packages/plugin" ]
    [ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "pnpm" ]
    [ "$(basename "$HOMEBOY_PROJECT_ADAPTER_MANIFEST")" = "nodejs.json" ]
    [ "$(homeboy_project_run_script_command test)" = "pnpm run test" ]
' _ "$PROJECT_SCRIPTS" "$WORKSPACE_PROJECT"

COMPOSER_PROJECT="$TMP_DIR/composer-project"
mkdir -p "$COMPOSER_PROJECT/src"
cat > "$COMPOSER_PROJECT/composer.json" <<'EOF'
{"scripts":{"test":"phpunit"}}
EOF

bash -c '
    source "$1"
    homeboy_project_init --ecosystem composer --path "$2/src"
    [ "$HOMEBOY_PROJECT_ECOSYSTEM" = "php" ]
    [ "$HOMEBOY_PROJECT_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_PACKAGE_MANAGER" = "composer" ]
    [ "$(basename "$HOMEBOY_PROJECT_ADAPTER_MANIFEST")" = "composer.json" ]
    [ "$(homeboy_project_run_script_command test)" = "composer run-script test" ]
    [ "$(homeboy_project_exec_command phpunit --filter Example)" = "composer exec phpunit --filter Example" ]
    homeboy_project_has_script test
    ! homeboy_project_has_script build
' _ "$PROJECT_SCRIPTS" "$COMPOSER_PROJECT"

FAKE_BIN="$TMP_DIR/bin"
COMMAND_LOG="$TMP_DIR/dependency-commands.log"
mkdir -p "$FAKE_BIN"
cat > "$FAKE_BIN/npm" <<'EOF'
#!/usr/bin/env bash
printf 'npm %s\n' "$*" >> "$HOMEBOY_DEPENDENCY_COMMAND_LOG"
EOF
cat > "$FAKE_BIN/composer" <<'EOF'
#!/usr/bin/env bash
printf 'composer %s\n' "$*" >> "$HOMEBOY_DEPENDENCY_COMMAND_LOG"
EOF
chmod +x "$FAKE_BIN/npm" "$FAKE_BIN/composer"

NPM_PROJECT="$TMP_DIR/npm-project"
mkdir -p "$NPM_PROJECT"
cat > "$NPM_PROJECT/package.json" <<'EOF'
{"scripts":{}}
EOF

PATH="$FAKE_BIN:$PATH" HOMEBOY_DEPENDENCY_COMMAND_LOG="$COMMAND_LOG" bash -c '
    source "$1"
    homeboy_project_init --ecosystem nodejs --path "$2"
    homeboy_project_ensure_dependencies
' _ "$PROJECT_SCRIPTS" "$NPM_PROJECT"
grep -q '^npm install$' "$COMMAND_LOG"

PATH="$FAKE_BIN:$PATH" HOMEBOY_DEPENDENCY_COMMAND_LOG="$COMMAND_LOG" bash -c '
    source "$1"
    homeboy_project_init --ecosystem composer --path "$2"
    homeboy_project_ensure_dependencies
' _ "$PROJECT_SCRIPTS" "$COMPOSER_PROJECT"
grep -q '^composer update$' "$COMMAND_LOG"

WORDPRESS_PROJECT="$TMP_DIR/wordpress-project"
mkdir -p "$WORDPRESS_PROJECT/wp-content/plugins/example" "$WORDPRESS_PROJECT/wp-includes"
cat > "$WORDPRESS_PROJECT/package.json" <<'EOF'
{"scripts":{"build":"wp-scripts build"}}
EOF
cat > "$WORDPRESS_PROJECT/composer.json" <<'EOF'
{"scripts":{"lint":"phpcs"}}
EOF

bash -c '
    source "$1"
    homeboy_project_init --ecosystem wordpress --path "$2/wp-content/plugins/example"
    [ "$HOMEBOY_PROJECT_ECOSYSTEM" = "wordpress" ]
    [ "$HOMEBOY_PROJECT_ROOT" = "$2" ]
    [ "$HOMEBOY_PROJECT_DEPENDENCY_ROOT" = "$2" ]
    [ "$(basename "$HOMEBOY_PROJECT_ADAPTER_MANIFEST")" = "wordpress.json" ]
' _ "$PROJECT_SCRIPTS" "$WORDPRESS_PROJECT"

WORDPRESS_INCOMPLETE="$TMP_DIR/wordpress-incomplete"
mkdir -p "$WORDPRESS_INCOMPLETE/wp-content/plugins/example"
if bash -c 'source "$1"; homeboy_project_init --ecosystem wordpress --path "$2/wp-content/plugins/example"' _ "$PROJECT_SCRIPTS" "$WORDPRESS_INCOMPLETE" 2>/dev/null; then
    echo "WordPress adapter should require all root signals from the manifest" >&2
    exit 1
fi
