#!/usr/bin/env bash

# Universal Build Script for WordPress Plugins and Themes
#
# Automatically detects project type from headers (Plugin Name or Theme Name)
# and creates standardized production builds with dependency management.
#
# Output Structure (assistant-instructions compliant):
#   /build/[project-name]/       - Clean production directory
#   /build/[project-name].zip    - Production ZIP file (non-versioned)
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

# Cleanup on exit (restore dev deps if build fails unexpectedly)
cleanup() {
    local exit_code=$?
    if [ -d "build/$PROJECT_NAME" ] && [ $exit_code -ne 0 ]; then
        rm -rf "build/$PROJECT_NAME"
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

# Module path from Homeboy environment (required)
MODULE_PATH="${HOMEBOY_MODULE_PATH}"

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
clean_previous_builds() {
    print_status "Cleaning previous build artifacts..."

    if [ -d "build" ]; then
        rm -rf build
    fi

    # Also clean old dist directories if they exist
    if [ -d "dist" ]; then
        print_warning "Removing legacy dist directory"
        rm -rf dist
    fi

    print_success "Previous builds cleaned"
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

# Build frontend assets (Gutenberg blocks via @wordpress/scripts, or Vite)
build_frontend_assets() {
    print_status "Checking for frontend build requirements..."

    # Check if package.json exists
    if [ ! -f "package.json" ]; then
        print_status "No package.json found, skipping frontend build"
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
    else
        print_status "No recognized build tool found, skipping frontend build"
        return 0
    fi

    # Check if node_modules exists, install if missing
    if [ ! -d "node_modules" ]; then
        print_status "Installing npm dependencies..."
        npm install --quiet 2>&1
    fi

    # Run the build command
    print_status "Building frontend assets..."
    npm run build --quiet 2>&1

    if [ $? -eq 0 ]; then
        print_success "Frontend assets built successfully ($build_tool)"
    else
        print_error "Frontend build failed"
        exit 1
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
            # Install dependencies if node_modules doesn't exist
            if [ ! -d "node_modules" ]; then
                print_status "  Installing dependencies for $pkg_dir..."
                npm ci --silent --no-audit --no-fund 2>&1
            fi

            # Run build
            print_status "  Running build for $pkg_dir..."
            if npm run build --silent 2>&1; then
                print_success "  Built $pkg_dir successfully"
            else
                print_error "  Build failed for $pkg_dir"
                cd - > /dev/null
                exit 1
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
/build/
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
}

# Copy files to build directory
copy_project_files() {
    print_status "Copying project files to build directory..."

    local build_dir="build/$PROJECT_NAME"
    mkdir -p "$build_dir"

    # Create rsync excludes file
    local exclude_file="/tmp/.rsync-excludes-$$"
    create_rsync_excludes "$exclude_file"

    # Copy files using rsync with excludes
    rsync -av --exclude-from="$exclude_file" ./ "$build_dir/" --quiet

    # Clean up exclude file
    rm -f "$exclude_file"

    print_success "Project files copied successfully"
}

# Validate build structure
validate_build() {
    print_status "Validating build structure..."

    local build_dir="build/$PROJECT_NAME"

    # Check main file exists
    if [ ! -f "$build_dir/$PROJECT_MAIN_FILE" ]; then
        print_error "Main file not found in build: $PROJECT_MAIN_FILE"
        return 1
    fi

    if [ "$PROJECT_TYPE" = "plugin" ]; then
        # Plugin validation: Check for common directories
        local found_dirs=false
        for dir in "inc" "includes" "assets" "src"; do
            if [ -d "$build_dir/$dir" ]; then
                found_dirs=true
                break
            fi
        done

        if [ "$found_dirs" = false ]; then
            print_warning "No standard plugin directories found (inc, includes, assets, src)"
        fi

    elif [ "$PROJECT_TYPE" = "theme" ]; then
        # Theme validation: Check for essential files
        local required_files=("index.php" "style.css")
        local missing_files=()

        for file in "${required_files[@]}"; do
            if [ ! -f "$build_dir/$file" ]; then
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
        SKIP_TESTS="${HOMEBOY_SKIP_TESTS:-}"
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
            if ! bash "${MODULE_PATH}/scripts/test-runner.sh"; then
                print_error "Test pipeline failed. See error details above."
                return 1
            fi
            print_success "Tests passed"
        fi
    fi
    return 0
}

# PHP syntax validation (runs on built files)
validate_php_syntax() {
    print_status "Running PHP syntax check on build..."

    local build_dir="build/$PROJECT_NAME"
    local php_errors=0

    while IFS= read -r -d '' file; do
        if ! php -l "$file" > /dev/null 2>&1; then
            php -l "$file" 2>&1
            php_errors=1
        fi
    done < <(find "$build_dir" -name "*.php" -print0)

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

    local zip_file="build/$PROJECT_NAME.zip"
    local build_dir="build/$PROJECT_NAME"

    # Remove existing ZIP if it exists
    if [ -f "$zip_file" ]; then
        rm -f "$zip_file"
    fi

    # Create ZIP from build directory (must be in build dir for correct paths)
    cd build
    zip -r "$PROJECT_NAME.zip" "$PROJECT_NAME/" -q
    cd - > /dev/null

    # Get file size
    local file_size=$(ls -lh "$zip_file" | awk '{print $5}')

    # Show contents summary
    local total_files=$(unzip -l "$zip_file" | tail -1 | awk '{print $2}')

    print_success "Production ZIP created: $zip_file ($file_size, $total_files files)"

    # Clean up intermediate directory now that ZIP is created
    print_status "Cleaning up intermediate build directory..."
    rm -rf "$build_dir"
    print_success "Intermediate directory removed (production files are in ZIP)"
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
        rm -rf "build/$PROJECT_NAME"
        restore_dev_deps
        exit 1
    fi

    if ! validate_build; then
        print_error "Build validation failed"
        restore_dev_deps
        exit 1
    fi

    # Validate PSR-4 autoload paths
    if [ -f "${MODULE_PATH}/scripts/build/validate-psr4.sh" ]; then
        if ! bash "${MODULE_PATH}/scripts/build/validate-psr4.sh" "build/$PROJECT_NAME"; then
            print_error "PSR-4 autoload validation failed"
            rm -rf "build/$PROJECT_NAME"
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

    check_dependencies
    detect_project
    extract_metadata
    build_project

    echo ""
    print_status "Build complete!"
    echo ""
}

# Run the main function
main "$@"
