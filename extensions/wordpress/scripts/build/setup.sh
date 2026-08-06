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
    write_github_env() {
        local name="$1"
        local value="$2"

        if [ -n "${GITHUB_ENV:-}" ]; then
            echo "${name}=${value}" >> "${GITHUB_ENV}"
        fi
    }

    configure_core_module() {
        local module_path="$1"

        if [ ! -f "${module_path}" ]; then
            return 1
        fi
        if ! node -e 'const value=require(process.argv[1]); const module=value?.default && typeof value.default === "object" ? {...value.default,...value} : value; if (typeof module?.runtimeContractManifest !== "function") process.exit(1);' "${module_path}" >/dev/null 2>&1; then
            return 1
        fi

        export HOMEBOY_WP_CODEBOX_CORE_MODULE="${module_path}"
        write_github_env "HOMEBOY_WP_CODEBOX_CORE_MODULE" "${module_path}"
        echo "WP Codebox core module configured: ${module_path}"
        return 0
    }

    probe_wp_codebox_runtime() {
        local bin_path="$1"

        # `commands` verifies the CLI entrypoint without starting a WordPress workload.
        if ! "${bin_path}" commands >/dev/null 2>&1; then
            echo "WP Codebox CLI runtime probe failed: ${bin_path}" >&2
            return 1
        fi

        return 0
    }

    probe_wp_codebox_native_runtime() {
        local dependency_root="$1"

        # Resolve from WP Codebox's dependency tree so Homeboy's dependencies
        # cannot satisfy this check. Requiring sharp loads its platform binary.
        if ! node -e 'const root=process.argv[1]; require(require.resolve("sharp", { paths: [ root ] }));' "${dependency_root}" >/dev/null 2>&1; then
            echo "WP Codebox native runtime probe failed to load sharp from: ${dependency_root}" >&2
            return 1
        fi

        return 0
    }

    wp_codebox_dependency_root() {
        local bin_path="$1"
        local install_root="${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-${HOME}/.cache/homeboy/wp-codebox}"
        local core_module="${HOMEBOY_WP_CODEBOX_CORE_MODULE:-}"

        case "${core_module}" in
            "${install_root}/source/node_modules/"*) printf '%s' "${install_root}/source" ;;
            "${install_root}/release/wp-codebox-cli/node_modules/"*) printf '%s' "${install_root}/release/wp-codebox-cli" ;;
            *) dirname "${bin_path}" ;;
        esac
    }

    first_non_empty_env() {
        local name
        for name in "$@"; do
            if [ -n "${!name:-}" ]; then
                printf '%s' "${!name}"
                return 0
            fi
        done
        return 1
    }

    is_managed_install_path() {
        local candidate="$1"
        local install_root="${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-${HOME}/.cache/homeboy/wp-codebox}"

        case "${candidate}" in
            "${install_root}"/*) return 0 ;;
            *) return 1 ;;
        esac
    }

    configure_explicit_overrides() {
        local explicit_bin=""
        local explicit_core_module=""
        local configured_bin=0
        local configured_core_module=0

        explicit_bin="$(first_non_empty_env HOMEBOY_WP_CODEBOX_CLI WP_CODEBOX_CLI WP_CODEBOX_BIN || true)"
        if [ -n "${explicit_bin}" ]; then
            if [ ! -x "${explicit_bin}" ]; then
                echo "Explicit WP Codebox CLI override is not executable: ${explicit_bin}" >&2
                exit 1
            fi
            export HOMEBOY_WP_CODEBOX_BIN="${explicit_bin}"
            write_github_env "HOMEBOY_WP_CODEBOX_BIN" "${explicit_bin}"
            echo "WP Codebox CLI override configured: ${explicit_bin}"
            configured_bin=1
        fi

        explicit_core_module="$(first_non_empty_env WP_CODEBOX_CORE_MODULE HOMEBOY_WP_CODEBOX_CORE_MODULE || true)"
        if [ -n "${explicit_core_module}" ]; then
            configure_core_module "${explicit_core_module}" || {
                if is_managed_install_path "${explicit_core_module}"; then
                    echo "Managed WP Codebox core module is missing; reinstalling: ${explicit_core_module}" >&2
                    unset WP_CODEBOX_CORE_MODULE HOMEBOY_WP_CODEBOX_CORE_MODULE
                    explicit_core_module=""
                else
                    echo "Explicit WP Codebox core module override is not a file: ${explicit_core_module}" >&2
                    exit 1
                fi
            }
            if [ -n "${explicit_core_module}" ]; then
                configured_core_module=1
            fi
        fi

        if [ "${configured_bin}" -eq 1 ] && { [ "${configured_core_module}" -eq 1 ] || resolve_core_module_from_known_locations; } && probe_wp_codebox_runtime "${HOMEBOY_WP_CODEBOX_BIN}" && probe_wp_codebox_native_runtime "$(wp_codebox_dependency_root "${HOMEBOY_WP_CODEBOX_BIN}")"; then
            node "${EXTENSION_PATH}/scripts/build/persist-wp-codebox-overrides.mjs" "${EXTENSION_PATH}/wordpress.json"
            return 0
        fi

        return 1
    }

    # Re-derive the core runtime module from the deterministic install
    # locations on disk. The CLI binary is persisted across GitHub Actions
    # steps via GITHUB_ENV, but HOMEBOY_WP_CODEBOX_CORE_MODULE does not always
    # survive process/step boundaries (e.g. `homeboy extension setup` runs in a
    # child process, and a cached bin can be picked up in a later step where the
    # earlier export is gone). The recipe loader hard-fails without the module,
    # so probe the known on-disk paths instead of trusting the env var.
    resolve_core_module_from_known_locations() {
        local probe_root="${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-${HOME}/.cache/homeboy/wp-codebox}"
        local candidate
        for candidate in \
            "${WP_CODEBOX_CORE_MODULE:-}" \
            "${HOMEBOY_WP_CODEBOX_CORE_MODULE:-}" \
            "${probe_root}/source/node_modules/@automattic/wp-codebox-core/dist/contracts.js" \
            "${probe_root}/source/node_modules/@automattic/wp-codebox-core/dist/index.js" \
            "${probe_root}/release/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist/contracts.js" \
            "${probe_root}/release/wp-codebox-cli/node_modules/@automattic/wp-codebox-core/dist/index.js"; do
            if [ -n "${candidate}" ] && configure_core_module "${candidate}"; then
                return 0
            fi
        done
        return 1
    }

    if configure_explicit_overrides; then
        return 0
    fi

    local source_install_requested=0
    if [ -n "${HOMEBOY_WP_CODEBOX_SOURCE:-}" ] || [ -n "${HOMEBOY_WP_CODEBOX_REF:-}" ] || [ "${HOMEBOY_WP_CODEBOX_INSTALL_MODE:-}" = "source" ]; then
        source_install_requested=1
        export HOMEBOY_WP_CODEBOX_INSTALL_MODE="source"
    fi

    if [ "${source_install_requested}" -eq 0 ] && [ -n "${HOMEBOY_WP_CODEBOX_BIN:-}" ] && [ -x "${HOMEBOY_WP_CODEBOX_BIN}" ]; then
        echo "WP Codebox already configured: ${HOMEBOY_WP_CODEBOX_BIN}"
        if resolve_core_module_from_known_locations && probe_wp_codebox_runtime "${HOMEBOY_WP_CODEBOX_BIN}" && probe_wp_codebox_native_runtime "$(wp_codebox_dependency_root "${HOMEBOY_WP_CODEBOX_BIN}")"; then
            return 0
        fi
        echo "WP Codebox CLI is configured without a ready runtime; (re)installing source module" >&2
    fi

    if [ "${source_install_requested}" -eq 0 ] && command -v wp-codebox >/dev/null 2>&1; then
        local detected_bin
        detected_bin="$(command -v wp-codebox)"
        echo "WP Codebox already available: ${detected_bin}"
        write_github_env "HOMEBOY_WP_CODEBOX_BIN" "${detected_bin}"
        if resolve_core_module_from_known_locations && probe_wp_codebox_runtime "${detected_bin}" && probe_wp_codebox_native_runtime "$(wp_codebox_dependency_root "${detected_bin}")"; then
            return 0
        fi
        echo "WP Codebox CLI is available without a ready runtime; (re)installing source module" >&2
    fi

    local install_mode install_root bin_dir bin_path platform arch artifact_name download_url artifact_path extract_dir
    install_mode="${HOMEBOY_WP_CODEBOX_INSTALL_MODE:-release}"
    install_root="${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-${HOME}/.cache/homeboy/wp-codebox}"
    bin_dir="${HOME}/.local/bin"
    bin_path="${bin_dir}/wp-codebox"

    if [ "${install_mode}" != "source" ]; then
        local release_artifact_downloaded=0
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

        if command -v curl >/dev/null 2>&1 && curl -fsIL "${download_url}" >/dev/null 2>&1 && curl -fsSL "${download_url}" -o "${artifact_path}"; then
            release_artifact_downloaded=1
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

            write_github_env "HOMEBOY_WP_CODEBOX_BIN" "${bin_path}"
            write_github_env "PATH" "${bin_dir}:${PATH}"

            echo "WP Codebox installed: ${bin_path}"
            if resolve_core_module_from_known_locations && probe_wp_codebox_runtime "${bin_path}" && probe_wp_codebox_native_runtime "${extract_dir}/wp-codebox-cli"; then
                return 0
            fi

            echo "WP Codebox release artifact did not provide a ready runtime; falling back to source install" >&2
        fi

        if [ "${release_artifact_downloaded}" -eq 0 ]; then
            echo "WP Codebox release artifact not published at ${download_url}; falling back to source install" >&2
        fi
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

    if [ ! -f "${repo_dir}/package-lock.json" ] && [ ! -f "${repo_dir}/npm-shrinkwrap.json" ]; then
        echo "WP Codebox source install requires an npm lockfile (package-lock.json or npm-shrinkwrap.json) for deterministic npm ci: ${source}" >&2
        exit 1
    fi

    local source_sha
    source_sha="$(git -C "${repo_dir}" rev-parse HEAD)"
    export WP_CODEBOX_SOURCE_REF="${ref}"
    export WP_CODEBOX_SOURCE_SHA="${source_sha}"
    write_github_env "WP_CODEBOX_SOURCE_REF" "${ref}"
    write_github_env "WP_CODEBOX_SOURCE_SHA" "${source_sha}"

    npm --prefix "${repo_dir}" ci --quiet --no-fund --no-audit --include=optional
    npm --prefix "${repo_dir}" run build --silent

    resolve_core_module_from_known_locations || {
        echo "Built WP Codebox source did not contain the @automattic/wp-codebox-core package entrypoint" >&2
        exit 1
    }

    local source_bin_path
    source_bin_path="${repo_dir}/packages/cli/dist/index.js"
    if [ ! -x "${source_bin_path}" ]; then
        echo "Built WP Codebox source did not contain an executable CLI at ${source_bin_path}" >&2
        exit 1
    fi

    probe_wp_codebox_runtime "${source_bin_path}" || {
        echo "Built WP Codebox source CLI runtime is not ready" >&2
        exit 1
    }
    probe_wp_codebox_native_runtime "${repo_dir}" || {
        echo "Built WP Codebox source native runtime is not ready" >&2
        exit 1
    }

    bin_path="${source_bin_path}"

    write_github_env "HOMEBOY_WP_CODEBOX_BIN" "${bin_path}"
    write_github_env "PATH" "${bin_dir}:${PATH}"

    echo "WP Codebox installed: ${bin_path}"
}

echo "Setting up WordPress extension..."

# Install PHP dev dependencies (PHPCS, PHPStan, PHPUnit - used for linting
# and the extension's own self-tests, not for running component tests).
if [ -f "composer.json" ]; then
    bash "${EXTENSION_PATH}/scripts/build/install-composer-dependencies.sh"

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
