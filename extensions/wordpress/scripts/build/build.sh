#!/usr/bin/env bash

# Universal Build Script for WordPress Plugins and Themes
#
# Automatically detects project type from headers (Plugin Name or Theme Name)
# and creates standardized production builds with dependency management.
#
# Output Structure:
#   /build/[project-name].zip    - Production ZIP file
#
# Staging uses .homeboy-build/ to avoid colliding with @wordpress/scripts
# which outputs to build/ by convention. The staging dir is cleaned up after
# the ZIP is created.
#
# Features:
# - Auto-detects plugin/theme from file headers
# - Extracts version for validation and logging
# - Installs production dependencies (composer --no-dev)
# - Builds Gutenberg blocks (@wordpress/scripts support)
# - Copies files using rsync with .buildignore exclusions
# - Validates build structure before packaging
# - Restores dev dependencies after build
#
# Usage: Run from plugin or theme directory: ./build.sh

set -e

# Staging directory for the intermediate production copy.
# Intentionally NOT build/ — @wordpress/scripts outputs to build/ by convention
# and using the same directory causes compiled JS/CSS to be silently dropped.
STAGING_ROOT=".homeboy-build"

# Cleanup on exit (restore dev deps if build fails unexpectedly)
cleanup() {
    local exit_code=$?
    if [ -d "${STAGING_ROOT}/${PROJECT_NAME:-}" ] && [ $exit_code -ne 0 ]; then
        rm -rf "${STAGING_ROOT}/${PROJECT_NAME}"
    fi
    if [ -f "composer.json" ]; then
        composer install --no-interaction --quiet 2>&1 || true
    fi
    exit $exit_code
}
trap cleanup EXIT

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Resolve execution context.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:?HOMEBOY_RUNTIME_RESOLVE_CONTEXT is required}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

# Output functions
print_status() {
    echo -e "${BLUE}[BUILD]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

npm_install_uses_legacy_peer_deps() {
    case "${HOMEBOY_NPM_LEGACY_PEER_DEPS:-auto}" in
        1|true|TRUE|yes|YES)
            return 0
            ;;
        0|false|FALSE|no|NO)
            return 1
            ;;
    esac

    [ -f "package.json" ] || return 1
    command -v node >/dev/null 2>&1 || return 1

    node <<'NODE'
const fs = require('fs');

let pkg;
try {
  pkg = JSON.parse(fs.readFileSync('package.json', 'utf8'));
} catch {
  process.exit(1);
}

const deps = {
  ...(pkg.dependencies || {}),
  ...(pkg.devDependencies || {}),
  ...(pkg.peerDependencies || {}),
};

function major(name) {
  const value = deps[name];
  if (!value) return null;
  const match = String(value).match(/\d+/);
  return match ? Number(match[0]) : null;
}

const needsLegacyPeerDeps =
  major('react') >= 19 ||
  major('@wordpress/scripts') >= 32 ||
  major('@wordpress/components') >= 34;

process.exit(needsLegacyPeerDeps ? 0 : 1);
NODE
}

npm_install_flags() {
    local flags=("--no-audit" "--no-fund")
    if npm_install_uses_legacy_peer_deps; then
        flags+=("--legacy-peer-deps")
    fi
    printf '%s\n' "${flags[@]}"
}

# Decide whether package-lock.json is an authoritative, committed artifact.
#
# `npm ci` strictly enforces that package.json and package-lock.json are in
# sync and aborts otherwise. That guarantee is only meaningful when the
# lockfile is committed to git — then it is the reproducible source of truth.
#
# When the lockfile is gitignored (or otherwise untracked), it is just a local
# build artifact that nothing keeps in sync with package.json. A stale leftover
# lockfile from a prior install then makes `npm ci` fail for a desync that no
# committed change could ever fix (e.g. a dependency was bumped in package.json
# but the ignored lockfile was never regenerated). In that case `npm install`
# is correct: it refreshes the local lockfile to match package.json.
#
# Returns 0 (true) when package-lock.json is tracked by git → use `npm ci`.
# Returns 1 (false) when missing, untracked, or gitignored → use `npm install`.
npm_lockfile_is_committed() {
    [ -f "package-lock.json" ] || return 1
    command -v git >/dev/null 2>&1 || return 1
    git rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 1
    # A tracked file is listed by ls-files; gitignored/untracked files are not.
    [ -n "$(git ls-files --error-unmatch package-lock.json 2>/dev/null)" ]
}

run_npm_install() {
    local command_name="$1"
    shift

    local flags=()
    while IFS= read -r flag; do
        flags+=("$flag")
    done < <(npm_install_flags)

    npm "$command_name" "$@" "${flags[@]}"
}

is_core_dev_project() {
    [ "${HOMEBOY_COMPONENT_SHAPE:-}" = "core-dev" ] && return 0
    [ -f "wp-config-sample.php" ] && [ -f "src/wp-includes/version.php" ] && [ -d "tests/phpunit" ]
}

build_core_dev_project() {
    print_status "WordPress core-dev build"

    if [ "${HOMEBOY_CORE_DEV_DRY_RUN:-}" = "1" ]; then
        print_status "core-dev build runner selected: $(pwd)"
        return 0
    fi

    if [ ! -d node_modules ]; then
        command -v npm >/dev/null 2>&1 || { print_error "npm is required to build wordpress-develop"; exit 1; }
        print_status "Installing npm dependencies..."
        run_npm_install install
    fi

    print_status "Running WordPress core build..."
    npm run build
    print_success "WordPress core build completed"
}

# Check for required tools
check_dependencies() {
    print_status "Checking build dependencies..."

    local missing_tools=()

    if ! command -v rsync &> /dev/null; then
        missing_tools+=("rsync")
    fi

    if ! command -v zip &> /dev/null; then
        missing_tools+=("zip")
    fi

    if [ ${#missing_tools[@]} -ne 0 ]; then
        print_error "Missing required tools: ${missing_tools[*]}"
        print_error "Please install the missing tools and try again."
        exit 1
    fi

    print_success "All build dependencies found"
}

# Detect project type and main file
detect_project() {
    print_status "Detecting project type..."

    # Look for plugin file (*.php with Plugin Name header)
    local plugin_file=$(find . -maxdepth 1 -name "*.php" -type f -exec grep -l "Plugin Name:" {} \; | head -1)

    if [ -n "$plugin_file" ]; then
        PROJECT_TYPE="plugin"
        PROJECT_MAIN_FILE=$(basename "$plugin_file")
        print_success "Detected WordPress plugin: $PROJECT_MAIN_FILE"
        return 0
    fi

    # Look for theme (style.css with Theme Name header)
    if [ -f "style.css" ] && grep -q "Theme Name:" "style.css"; then
        PROJECT_TYPE="theme"
        PROJECT_MAIN_FILE="style.css"
        print_success "Detected WordPress theme"
        return 0
    fi

    print_error "Could not detect project type (plugin or theme)"
    print_error "Expected: *.php with 'Plugin Name:' header OR style.css with 'Theme Name:' header"
    exit 1
}

# Extract project metadata
extract_metadata() {
    print_status "Extracting project metadata..."

    if [ "$PROJECT_TYPE" = "plugin" ]; then
        # Extract plugin name from filename (remove .php extension)
        PROJECT_NAME="${PROJECT_MAIN_FILE%.php}"

        # Extract version from plugin header
        PROJECT_VERSION=$(grep -i "Version:" "$PROJECT_MAIN_FILE" | head -1 | sed 's/.*Version:[ ]*\([0-9\.]*\).*/\1/')

    elif [ "$PROJECT_TYPE" = "theme" ]; then
        # Extract theme name from directory name
        PROJECT_NAME=$(basename "$PWD")

        # Extract version from theme header
        PROJECT_VERSION=$(grep -i "Version:" "$PROJECT_MAIN_FILE" | head -1 | sed 's/.*Version:[ ]*\([0-9\.]*\).*/\1/')
    fi

    # When invoked by homeboy, use the component ID for artifact naming so the
    # zip matches the deploy system's artifact_pattern (build/{component_id}.zip).
    # Falls back to the auto-detected PROJECT_NAME for standalone usage.
    if [ -n "$HOMEBOY_COMPONENT_ID" ]; then
        PROJECT_NAME="$HOMEBOY_COMPONENT_ID"
    fi

    if [ -z "$PROJECT_NAME" ]; then
        print_error "Could not extract project name"
        exit 1
    fi

    if [ -z "$PROJECT_VERSION" ]; then
        print_error "Could not extract version from $PROJECT_MAIN_FILE"
        exit 1
    fi

    print_success "Project: $PROJECT_NAME v$PROJECT_VERSION"
}

# Clean previous builds
#
# Cleans the staging directory and any previous ZIP artifact.
# Does NOT clean build/ — that's where @wordpress/scripts outputs compiled
# JS/CSS, and npm run build needs it to survive between steps.
#
# Handles stale build directories that may be owned by a different user
# (e.g., from a previous deployment or migration). Tries progressively
# stronger cleanup strategies before failing with an actionable message.
clean_previous_builds() {
    print_status "Cleaning previous build artifacts..."

    remove_stale_dir "$STAGING_ROOT"

    # Remove previous ZIP artifact (lives in build/ alongside npm output)
    if [ -n "$PROJECT_NAME" ] && [ -f "build/$PROJECT_NAME.zip" ]; then
        rm -f "build/$PROJECT_NAME.zip"
    fi

    # Also clean old dist directories if they exist
    if [ -d "dist" ]; then
        print_warning "Removing legacy dist directory"
        remove_stale_dir "dist"
    fi

    print_success "Previous builds cleaned"
}

# Remove a directory that may be owned by a different user.
# Strategy: rm -rf → chmod + rm -rf → actionable error.
remove_stale_dir() {
    local dir="$1"
    [ -d "$dir" ] || return 0

    # Fast path: normal removal
    if rm -rf "$dir" 2>/dev/null; then
        return 0
    fi

    # Stale ownership detected. Try fixing permissions first.
    print_warning "Cannot remove $dir/ (permission denied). Attempting permission fix..."
    chmod -R u+rwX "$dir" 2>/dev/null || true
    if rm -rf "$dir" 2>/dev/null; then
        print_success "Removed $dir/ after permission fix"
        return 0
    fi

    # Last resort: if running as root or sudo is available, use it
    if [ "$(id -u)" = "0" ]; then
        # Already root but rm failed — shouldn't happen, but try force
        rm -rf "$dir"
        return $?
    fi

    print_error "Cannot remove stale $dir/ directory (owned by a different user)."
    print_error "Fix manually: sudo rm -rf $(pwd)/$dir"
    exit 1
}

# Install production dependencies
install_production_deps() {
    print_status "Installing production dependencies..."

    if [ -f "composer.json" ]; then
        composer install --no-dev --optimize-autoloader --no-interaction --quiet 2>&1
        print_success "Production dependencies installed"
    else
        print_warning "No composer.json found, skipping Composer dependencies"
    fi
}

# Restore development dependencies
restore_dev_deps() {
    print_status "Restoring development dependencies..."

    if [ -f "composer.json" ]; then
        composer install --no-interaction --quiet 2>&1
        print_success "Development dependencies restored"
    fi
}

# Install npm dependencies if missing/stale for detected build tool.
install_frontend_dependencies() {
    local build_tool="$1"
    local scope_label="$2"
    local expected_bin=""
    local need_install=0

    case "$build_tool" in
        wordpress-scripts)
            expected_bin="wp-scripts"
            ;;
        vite)
            expected_bin="vite"
            ;;
    esac

    if [ ! -d "node_modules" ]; then
        print_status "${scope_label}Installing npm dependencies..."
        need_install=1
    elif [ -f "package-lock.json" ] && [ ! -f "node_modules/.package-lock.json" ]; then
        print_warning "${scope_label}node_modules exists but npm install state cannot be verified. Reinstalling dependencies..."
        need_install=1
    elif [ -f "package-lock.json" ] && [ "package-lock.json" -nt "node_modules/.package-lock.json" ]; then
        print_warning "${scope_label}package-lock.json is newer than node_modules. Reinstalling dependencies..."
        need_install=1
    elif [ -f "package-lock.json" ] && ! npm ls --depth=0 $(npm_install_flags) >/dev/null 2>&1; then
        print_warning "${scope_label}node_modules contains missing or invalid packages. Reinstalling dependencies..."
        need_install=1
    elif [ -n "$expected_bin" ] && [ ! -x "node_modules/.bin/$expected_bin" ]; then
        print_warning "${scope_label}node_modules exists but '$expected_bin' is missing. Reinstalling dependencies..."
        need_install=1
    fi

    if [ "$need_install" -eq 1 ]; then
        # Only use `npm ci` when the lockfile is a committed, authoritative
        # artifact. A gitignored/untracked lockfile is a local leftover that
        # nothing keeps in sync with package.json — `npm ci` would fail on a
        # desync no committed change could fix. Fall back to `npm install`,
        # which refreshes the local lockfile to match package.json.
        if npm_lockfile_is_committed; then
            run_npm_install ci 2>&1
        else
            if [ -f "package-lock.json" ]; then
                print_warning "${scope_label}package-lock.json is not committed (gitignored or untracked); using 'npm install' to refresh it instead of 'npm ci'."
            fi
            run_npm_install install 2>&1
        fi
        print_success "${scope_label}npm dependencies ready"
    fi
}

# Build frontend assets (Gutenberg blocks via @wordpress/scripts, Vite, or generic npm build)
#
# Frontend builds are non-fatal for PHP-primary plugins. If node/npm is
# unavailable or the build fails, the script warns but continues — the PHP
# plugin still works without built JS assets. Set HOMEBOY_REQUIRE_FRONTEND=1
# to make frontend builds fatal (for JS-heavy projects where the build IS
# the deliverable).
build_frontend_assets() {
    print_status "Checking for frontend build requirements..."

    REQUIRE_FRONTEND="${HOMEBOY_REQUIRE_FRONTEND:-0}"

    # Check if package.json exists
    if [ ! -f "package.json" ]; then
        print_status "No package.json found, skipping frontend build"
        return 0
    fi

    # Check if node/npm are available
    if ! command -v node &> /dev/null || ! command -v npm &> /dev/null; then
        if [ "$REQUIRE_FRONTEND" = "1" ]; then
            print_error "node/npm not available but HOMEBOY_REQUIRE_FRONTEND=1"
            exit 1
        fi
        print_warning "node/npm not available, skipping frontend build (PHP-only artifact)"
        return 0
    fi

    # Determine build tool
    local build_tool=""
    if grep -q "@wordpress/scripts" "package.json"; then
        build_tool="wordpress-scripts"
        print_status "Detected @wordpress/scripts build tool"
    elif grep -q '"vite"' "package.json"; then
        build_tool="vite"
        print_status "Detected Vite build tool"
    elif node -e "const p=require('./package.json'); process.exit(p.scripts && p.scripts.build ? 0 : 1)" 2>/dev/null; then
        build_tool="npm"
        print_status "Detected npm build script"
    else
        print_status "No build script found, skipping frontend build"
        return 0
    fi

    # Ensure dependencies exist and expected local build binary is present.
    install_frontend_dependencies "$build_tool" ""

    # Run the build command
    print_status "Building frontend assets..."
    if npm run build --quiet 2>&1; then
        print_success "Frontend assets built successfully ($build_tool)"
    else
        if [ "$REQUIRE_FRONTEND" = "1" ]; then
            print_error "Frontend build failed (fatal: HOMEBOY_REQUIRE_FRONTEND=1)"
            exit 1
        fi
        print_warning "Frontend build failed — continuing with PHP-only artifact"
        print_warning "The plugin will work without JS assets. Fix the frontend build to include them."
        FRONTEND_BUILD_FAILED=1
    fi
}

# Build nested frontend assets (Data Machine pattern - multiple package.json in subdirectories)
build_nested_packages() {
    print_status "Checking for nested package.json files..."

    local nested_packages=()

    # Find directories with package.json (excluding node_modules)
    while IFS= read -r -d '' pkg_dir; do
        # Skip root package.json and node_modules
        if [ "$pkg_dir" != "." ] && [[ ! "$pkg_dir" =~ node_modules ]]; then
            nested_packages+=("$pkg_dir")
        fi
    done < <(find . -name "package.json" -not -path "*/node_modules/*" -exec dirname {} \; | sed 's|^\./||' | sort -u | while read -r dir; do
        if [ -n "$dir" ]; then
            printf '%s\0' "$dir"
        fi
    done)

    if [ ${#nested_packages[@]} -eq 0 ]; then
        print_status "No nested package.json files found"
        return 0
    fi

    print_status "Found ${#nested_packages[@]} nested package(s) to build"

    for pkg_dir in "${nested_packages[@]}"; do
        print_status "Building nested package: $pkg_dir"

        cd "$pkg_dir"

        # Check if it has a build script
        if grep -q '"build"' "package.json"; then
            # Detect common build tools for better stale node_modules handling.
            local nested_build_tool=""
            if grep -q "@wordpress/scripts" "package.json"; then
                nested_build_tool="wordpress-scripts"
            elif grep -q '"vite"' "package.json"; then
                nested_build_tool="vite"
            fi

            install_frontend_dependencies "$nested_build_tool" "  "

            # Run build
            print_status "  Running build for $pkg_dir..."
            if npm run build --silent 2>&1; then
                print_success "  Built $pkg_dir successfully"
            else
                if [ "$REQUIRE_FRONTEND" = "1" ]; then
                    print_error "  Build failed for $pkg_dir (fatal: HOMEBOY_REQUIRE_FRONTEND=1)"
                    cd - > /dev/null
                    exit 1
                fi
                print_warning "  Build failed for $pkg_dir — continuing without it"
                FRONTEND_BUILD_FAILED=1
            fi
        else
            print_status "  No build script found in $pkg_dir, skipping"
        fi

        cd - > /dev/null
    done

    print_success "All nested packages built successfully"
}

# Create rsync exclude patterns
create_rsync_excludes() {
    local exclude_file="$1"

    if [ -f ".buildignore" ]; then
        # Convert .buildignore to rsync exclude format (preserve leading slash for root-only patterns)
        sed 's|/$||; /^#/d; /^$/d' .buildignore > "$exclude_file"
    else
        # Default excludes if no .buildignore file
        # Note: /build/ is intentionally NOT excluded — @wordpress/scripts outputs
        # compiled JS/CSS there, and it must be included in the production ZIP.
        # The staging directory (.homeboy-build/) is excluded instead.
        cat > "$exclude_file" << 'EOF'
.git
.gitignore
.gitattributes
README.md
CLAUDE.md
AGENTS.md
.claude
.vscode
.idea
*.swp
*.swo
*~
/.homeboy-build/
/dist/
*.zip
*.tar.gz
.DS_Store
._*
node_modules
*.log
*.tmp
*.temp
.env*
build.sh
.buildignore
/tests
phpunit.xml*
.github
composer.lock
package-lock.json
webpack.config.js
EOF
    fi

    # Homeboy-managed staging must never be copied into itself. Keep this
    # mandatory even when a project supplies a custom .buildignore.
    cat >> "$exclude_file" << 'EOF'
/.homeboy-build/
EOF
}

resolve_package_artifacts() {
    local manifest_file="$1"
    local list_file="$2"

    : > "$list_file"
    printf '%s\n' '{"type":"wordpress.package_artifacts","artifacts":[]}' > "$manifest_file"

    [ -n "${HOMEBOY_SETTINGS_JSON:-}" ] || return 0
    [ "${HOMEBOY_SETTINGS_JSON}" != "{}" ] || return 0

    HOMEBOY_WORDPRESS_PACKAGE_ARTIFACTS_MANIFEST="$manifest_file" \
    HOMEBOY_WORDPRESS_PACKAGE_ARTIFACTS_LIST="$list_file" \
    php <<'PHP'
<?php
$settings = json_decode( getenv( 'HOMEBOY_SETTINGS_JSON' ) ?: '{}', true );
if ( ! is_array( $settings ) ) {
	fwrite( STDERR, "Invalid HOMEBOY_SETTINGS_JSON; expected a JSON object.\n" );
	exit( 1 );
}

$patterns = $settings['package_artifacts'] ?? [];
if ( ! is_array( $patterns ) ) {
	fwrite( STDERR, "extensions.wordpress.package_artifacts must be an array of component-relative glob patterns.\n" );
	exit( 1 );
}

$artifacts = [];
$seen      = [];
$list      = '';

foreach ( $patterns as $pattern ) {
	if ( ! is_string( $pattern ) || '' === $pattern ) {
		fwrite( STDERR, "extensions.wordpress.package_artifacts entries must be non-empty strings.\n" );
		exit( 1 );
	}

	$normalized = str_replace( '\\', '/', $pattern );
	if ( str_starts_with( $normalized, '/' ) || preg_match( '#(^|/)\.\.(/|$)#', $normalized ) ) {
		fwrite( STDERR, "Package artifact patterns must be component-relative and cannot contain '..': {$pattern}\n" );
		exit( 1 );
	}

	$matches = glob( $pattern, GLOB_BRACE ) ?: [];
	$files   = [];
	foreach ( $matches as $match ) {
		if ( is_file( $match ) ) {
			$relative = str_replace( '\\', '/', $match );
			if ( str_starts_with( $relative, './' ) ) {
				$relative = substr( $relative, 2 );
			}
			$files[]  = $relative;
		}
	}

	if ( [] === $files ) {
		fwrite( STDERR, "Declared WordPress package artifact pattern matched no files: {$pattern}\n" );
		exit( 1 );
	}

	sort( $files );
	foreach ( $files as $relative ) {
		if ( isset( $seen[ $relative ] ) ) {
			continue;
		}
		$seen[ $relative ] = true;
		$sha256            = hash_file( 'sha256', $relative ) ?: '';
		$artifacts[]       = [
			'path'    => $relative,
			'sha256'  => $sha256,
			'pattern' => $pattern,
		];
		$list .= $relative . "\t" . $sha256 . "\n";
	}
}

$manifest = [
	'type'      => 'wordpress.package_artifacts',
	'artifacts' => $artifacts,
];

file_put_contents( getenv( 'HOMEBOY_WORDPRESS_PACKAGE_ARTIFACTS_MANIFEST' ), json_encode( $manifest, JSON_UNESCAPED_SLASHES ) . "\n" );
file_put_contents( getenv( 'HOMEBOY_WORDPRESS_PACKAGE_ARTIFACTS_LIST' ), $list );
PHP
}

include_package_artifacts() {
    local staging_dir="$1"
    local manifest_file="/tmp/.wordpress-package-artifacts-$$.json"
    local list_file="/tmp/.wordpress-package-artifacts-$$.tsv"

    resolve_package_artifacts "$manifest_file" "$list_file"

    if [ -s "$list_file" ]; then
        while IFS=$'\t' read -r relative_path sha256; do
            [ -n "$relative_path" ] || continue
            mkdir -p "$staging_dir/$(dirname "$relative_path")"
            cp "$relative_path" "$staging_dir/$relative_path"
            print_status "Included package artifact: $relative_path (sha256: $sha256)"
        done < "$list_file"

        cat "$manifest_file"
    fi

    rm -f "$manifest_file" "$list_file"
}

# Copy files to staging directory
copy_project_files() {
    print_status "Copying project files to staging directory..."

    local staging_dir="${STAGING_ROOT}/$PROJECT_NAME"
    mkdir -p "$staging_dir"

    # Create rsync excludes file
    local exclude_file="/tmp/.rsync-excludes-$$"
    create_rsync_excludes "$exclude_file"

    # Copy files using rsync with excludes
    rsync -av --exclude-from="$exclude_file" ./ "$staging_dir/" --quiet

    include_package_artifacts "$staging_dir"

    # Clean up exclude file
    rm -f "$exclude_file"

    print_success "Project files copied successfully"
}

# Validate build structure
validate_build() {
    print_status "Validating build structure..."

    local staging_dir="${STAGING_ROOT}/$PROJECT_NAME"

    # Check main file exists
    if [ ! -f "$staging_dir/$PROJECT_MAIN_FILE" ]; then
        print_error "Main file not found in build: $PROJECT_MAIN_FILE"
        return 1
    fi

    if [ "$PROJECT_TYPE" = "plugin" ]; then
        # Plugin validation: Check for common directories
        local found_dirs=false
        for dir in "inc" "includes" "assets" "src"; do
            if [ -d "$staging_dir/$dir" ]; then
                found_dirs=true
                break
            fi
        done

        if [ "$found_dirs" = false ]; then
            print_warning "No standard plugin directories found (inc, includes, assets, src)"
        fi

    elif [ "$PROJECT_TYPE" = "theme" ]; then
        # Theme validation: classic themes need index.php; block themes use
        # templates/index.html as the fallback template.
        local required_files=("style.css")
        if [ -f "$staging_dir/theme.json" ]; then
            required_files+=("templates/index.html")
        else
            required_files+=("index.php")
        fi

        local missing_files=()

        for file in "${required_files[@]}"; do
            if [ ! -f "$staging_dir/$file" ]; then
                missing_files+=("$file")
            fi
        done

        if [ ${#missing_files[@]} -ne 0 ]; then
            print_error "Essential theme files missing: ${missing_files[*]}"
            return 1
        fi
    fi

    print_success "Build structure validation passed"
    return 0
}

# Run tests (before production deps are installed)
run_tests() {
    if [ -f "composer.json" ] && grep -q '"test"' composer.json; then
        # Check for test override settings
        # Default: skip tests during builds. Use HOMEBOY_SKIP_TESTS=0 to force tests.
        # Tests should run via `homeboy test`, not during builds/deploys.
        SKIP_TESTS="${HOMEBOY_SKIP_TESTS:-1}"
        USE_LOCAL_TESTS="${HOMEBOY_USE_LOCAL_TESTS:-}"

        if [ "$SKIP_TESTS" = "true" ] || [ "$SKIP_TESTS" = "1" ]; then
            print_status "Skipping tests (HOMEBOY_SKIP_TESTS=$SKIP_TESTS)"
        elif [ "$USE_LOCAL_TESTS" = "true" ] || [ "$USE_LOCAL_TESTS" = "1" ]; then
            print_status "Using local test infrastructure (HOMEBOY_USE_LOCAL_TESTS=$USE_LOCAL_TESTS)"
            if [ -f "vendor/bin/phpunit" ]; then
                vendor/bin/phpunit --testdox 2>&1
            elif [ -f "tests/vendor/bin/phpunit" ]; then
                tests/vendor/bin/phpunit --testdox 2>&1
            else
                print_error "No local PHPUnit found. Run 'composer install' or install module's test infrastructure."
                return 1
            fi
        else
            # Run tests with module infrastructure (default)
            print_status "Running tests with module infrastructure..."
            if ! bash "${EXTENSION_PATH}/scripts/test/test-runner.sh"; then
                print_error "Test pipeline failed. See error details above."
                return 1
            fi
            print_success "Tests passed"
        fi
    fi
    return 0
}

# PHP syntax validation (runs on staged files)
validate_php_syntax() {
    print_status "Running PHP syntax check on build..."

    local staging_dir="${STAGING_ROOT}/$PROJECT_NAME"
    local staging_abs=""
    local php_file_list="/tmp/.wordpress-php-syntax-files-$$.list"
    local php_errors=0

    if [ ! -d "$staging_dir" ]; then
        print_error "Staging directory not found for PHP syntax validation: $staging_dir"
        return 1
    fi

    staging_abs="$(cd "$staging_dir" && pwd)"

    if ! find "$staging_abs" -type f -name "*.php" -print > "$php_file_list"; then
        rm -f "$php_file_list"
        print_error "Could not enumerate staged PHP files under: $staging_dir"
        return 1
    fi

    LC_ALL=C sort -o "$php_file_list" "$php_file_list"

    while IFS= read -r file; do
        [ -n "$file" ] || continue

        if [ ! -f "$file" ]; then
            print_error "Staged PHP file disappeared during syntax validation: ${file#$staging_abs/}"
            php_errors=1
            continue
        fi

        local lint_output=""
        if ! lint_output="$(php -l "$file" 2>&1)"; then
            printf '%s\n' "$lint_output"
            php_errors=1
        fi
    done < "$php_file_list"

    rm -f "$php_file_list"

    if [ $php_errors -eq 1 ]; then
        print_error "PHP syntax errors found. Build aborted."
        return 1
    fi

    print_success "PHP syntax check passed"
    return 0
}

# Create production ZIP
create_production_zip() {
    print_status "Creating production ZIP file..."

    # Ensure build/ output dir exists (may contain npm build output too)
    mkdir -p build

    local zip_file="build/$PROJECT_NAME.zip"
    local staging_dir="${STAGING_ROOT}/$PROJECT_NAME"

    # Remove existing ZIP if it exists
    if [ -f "$zip_file" ]; then
        rm -f "$zip_file"
    fi

    # Create ZIP from staging directory (must be in staging root for correct paths)
    cd "$STAGING_ROOT"
    zip -r "../$zip_file" "$PROJECT_NAME/" -q
    cd - > /dev/null

    # Get file size
    local file_size=$(ls -lh "$zip_file" | awk '{print $5}')

    # Show contents summary
    local total_files=$(unzip -l "$zip_file" | tail -1 | awk '{print $2}')

    print_success "Production ZIP created: $zip_file ($file_size, $total_files files)"

    # Clean up staging directory now that ZIP is created
    print_status "Cleaning up staging directory..."
    rm -rf "$staging_dir"
    print_success "Staging directory removed (production files are in ZIP)"
}

# Main build process
build_project() {
    print_status "Starting build process for $PROJECT_NAME v$PROJECT_VERSION"
    print_status "============================================="

    # Run tests FIRST while dev dependencies are still available
    if ! run_tests; then
        print_error "Tests failed"
        exit 1
    fi

    clean_previous_builds
    install_production_deps
    build_frontend_assets
    build_nested_packages
    copy_project_files

    if ! validate_php_syntax; then
        print_error "PHP syntax validation failed"
        rm -rf "${STAGING_ROOT}/$PROJECT_NAME"
        restore_dev_deps
        exit 1
    fi

    if ! validate_build; then
        print_error "Build validation failed"
        restore_dev_deps
        exit 1
    fi

    # Validate PSR-4 autoload paths
    if [ -f "${EXTENSION_PATH}/scripts/build/validate-psr4.sh" ]; then
        if ! bash "${EXTENSION_PATH}/scripts/build/validate-psr4.sh" "${STAGING_ROOT}/$PROJECT_NAME"; then
            print_error "PSR-4 autoload validation failed"
            rm -rf "${STAGING_ROOT}/$PROJECT_NAME"
            restore_dev_deps
            exit 1
        fi
    fi

    create_production_zip
    restore_dev_deps

    print_success "Build process completed successfully!"
    print_success "Production package: build/$PROJECT_NAME.zip"
    echo ""
    print_status "Need production files? Simply unzip the archive!"
}

# Main script execution
main() {
    echo ""
    print_status "Universal WordPress Build Script"
    print_status "================================="
    echo ""

    FRONTEND_BUILD_FAILED=0

    if is_core_dev_project; then
        build_core_dev_project
        return 0
    fi

    check_dependencies
    detect_project
    extract_metadata
    build_project

    if [ "$FRONTEND_BUILD_FAILED" = "1" ]; then
        echo ""
        print_warning "=========================================="
        print_warning "Build completed with frontend warnings"
        print_warning "PHP artifact was created successfully."
        print_warning "Frontend assets were NOT included."
        print_warning "Fix the frontend build to include JS/CSS."
        print_warning "=========================================="
    else
        echo ""
        print_status "Build complete!"
    fi
    echo ""
}

# Run the main function
main "$@"
