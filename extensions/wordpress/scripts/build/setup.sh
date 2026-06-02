#!/bin/bash
set -euo pipefail

# Setup script for WordPress Homeboy extension.
#
# Installs npm dependencies, PHP dev dependencies (PHPCS, PHPStan for linting),
# and the WP Codebox CLI used by the default WordPress test backend.
#
# The legacy wp-phpunit dependency was removed in Phase 3 (#214). WordPress
# PHPUnit execution now runs through WP Codebox by default. The host-smoke
# backend is only for standalone PHP smoke scripts.

EXTENSION_PATH="$(pwd)"

install_wp_codebox() {
    if [ -n "${HOMEBOY_WP_CODEBOX_BIN:-}" ] && [ -x "${HOMEBOY_WP_CODEBOX_BIN}" ]; then
        echo "WP Codebox already configured: ${HOMEBOY_WP_CODEBOX_BIN}"
        return 0
    fi

    if command -v wp-codebox >/dev/null 2>&1; then
        local detected_bin
        detected_bin="$(command -v wp-codebox)"
        echo "WP Codebox already available: ${detected_bin}"
        if [ -n "${GITHUB_ENV:-}" ]; then
            echo "HOMEBOY_WP_CODEBOX_BIN=${detected_bin}" >> "${GITHUB_ENV}"
        fi
        return 0
    fi

    local install_mode install_root bin_dir bin_path platform arch artifact_name download_url artifact_path extract_dir
    install_mode="${HOMEBOY_WP_CODEBOX_INSTALL_MODE:-release}"
    install_root="${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-${HOME}/.cache/homeboy/wp-codebox}"
    bin_dir="${HOME}/.local/bin"
    bin_path="${bin_dir}/wp-codebox"

    if [ "${install_mode}" != "source" ]; then
        platform="$(uname -s | tr '[:upper:]' '[:lower:]')"
        arch="$(uname -m)"
        case "${platform}" in
            darwin) platform="macos" ;;
        esac
        case "${arch}" in
            x86_64|amd64) arch="x64" ;;
            aarch64) arch="arm64" ;;
        esac

        artifact_name="wp-codebox-cli-${platform}-${arch}.tar.gz"
        download_url="${HOMEBOY_WP_CODEBOX_DOWNLOAD_URL:-https://github.com/Automattic/wp-codebox/releases/latest/download/${artifact_name}}"
        artifact_path="${install_root}/${artifact_name}"
        extract_dir="${install_root}/release"

        echo "Installing WP Codebox CLI from release artifact (${download_url})..."
        mkdir -p "${install_root}" "${bin_dir}"

        if command -v curl >/dev/null 2>&1 && curl -fsSL "${download_url}" -o "${artifact_path}"; then
            rm -rf "${extract_dir}"
            mkdir -p "${extract_dir}"
            tar -xzf "${artifact_path}" -C "${extract_dir}"

            if [ ! -x "${extract_dir}/wp-codebox-cli/bin/wp-codebox" ]; then
                echo "Downloaded WP Codebox artifact did not contain an executable bin/wp-codebox" >&2
                exit 1
            fi

            cat > "${bin_path}" <<EOF
#!/usr/bin/env bash
exec "${extract_dir}/wp-codebox-cli/bin/wp-codebox" "\$@"
EOF
            chmod +x "${bin_path}"

            if [ -n "${GITHUB_ENV:-}" ]; then
                echo "HOMEBOY_WP_CODEBOX_BIN=${bin_path}" >> "${GITHUB_ENV}"
                echo "PATH=${bin_dir}:${PATH}" >> "${GITHUB_ENV}"
            fi

            echo "WP Codebox installed: ${bin_path}"
            return 0
        fi

        echo "WP Codebox release artifact unavailable; falling back to source install" >&2
    fi

    local source ref repo_dir
    source="${HOMEBOY_WP_CODEBOX_SOURCE:-https://github.com/Automattic/wp-codebox.git}"
    ref="${HOMEBOY_WP_CODEBOX_REF:-main}"
    repo_dir="${install_root}/source"

    echo "Installing WP Codebox CLI (${source}@${ref})..."
    mkdir -p "${install_root}" "${bin_dir}"

    if [ ! -d "${repo_dir}/.git" ]; then
        rm -rf "${repo_dir}"
        git clone --quiet "${source}" "${repo_dir}"
    fi

    git -C "${repo_dir}" fetch --quiet origin "${ref}"
    git -C "${repo_dir}" checkout --quiet FETCH_HEAD

    npm --prefix "${repo_dir}" install --quiet --no-fund --no-audit --omit=optional
    npm --prefix "${repo_dir}" run build --silent

    cat > "${bin_path}" <<EOF
#!/usr/bin/env bash
exec node "${repo_dir}/packages/cli/dist/index.js" "\$@"
EOF
    chmod +x "${bin_path}"

    if [ -n "${GITHUB_ENV:-}" ]; then
        echo "HOMEBOY_WP_CODEBOX_BIN=${bin_path}" >> "${GITHUB_ENV}"
        echo "PATH=${bin_dir}:${PATH}" >> "${GITHUB_ENV}"
    fi

    echo "WP Codebox installed: ${bin_path}"
}

echo "Setting up WordPress extension..."

# Install PHP dev dependencies (PHPCS, PHPStan, PHPUnit — used for linting
# and the extension's own self-tests, not for running component tests).
if [ -f "composer.json" ]; then
    echo "Installing PHP dependencies..."
    composer install --quiet --no-interaction

    if [ -x "vendor/bin/phpcs" ]; then
        echo "Registering PHPCS standards..."
        phpcs_paths=()
        for path in \
            "${EXTENSION_PATH}/vendor/wp-coding-standards/wpcs" \
            "${EXTENSION_PATH}/vendor/phpcsstandards/phpcsextra" \
            "${EXTENSION_PATH}/vendor/phpcsstandards/phpcsutils" \
            "${EXTENSION_PATH}/HomeboyWordPress"; do
            if [ -d "$path" ]; then
                phpcs_paths+=("$path")
            fi
        done

        if [ "${#phpcs_paths[@]}" -gt 0 ]; then
            installed_paths=$(IFS=','; printf '%s' "${phpcs_paths[*]}")
            vendor/bin/phpcs --config-set installed_paths "$installed_paths" --quiet > /dev/null 2>&1
        fi
    fi
fi

# Install npm dependencies (Blueprint validation helpers, ESLint).
if [ -f "package.json" ]; then
    echo "Installing npm dependencies..."
    if [ -f "package-lock.json" ]; then
        npm ci --quiet --no-fund --no-audit 2>&1 || {
            echo "Warning: npm ci failed — extension Node tooling may not be available"
        }
    else
        npm install --quiet --no-fund --no-audit 2>&1 || {
            echo "Warning: npm install failed — extension Node tooling may not be available"
        }
    fi
fi

install_wp_codebox

echo "WordPress extension setup complete."
echo "Default test backend: WP Codebox (WordPress Playground runtime)"
echo "Host smoke backend: set test_backend=host-smoke for standalone tests/**/*-smoke.php"
