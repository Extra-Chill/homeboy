#!/usr/bin/env bash

# Local workspace dependency overrides for Node.js builds.
#
# Problem this solves
# -------------------
# A component can depend on a sibling workspace package that is intentionally
# NOT published to a registry — it is "cooked locally" on a branch (e.g. an
# `@scope/ui` package living in a sibling pnpm monorepo built on `trunk`).
# There is no clean way to make a build consume it:
#
#   * An npm `file:` install symlinks the dependency's own node_modules into the
#     consumer. The dependency's peer deps (React, etc.) then resolve to a
#     SECOND copy → "Invalid hook call" → a blank app.
#   * A hand-rolled tarball is manual and fragile.
#
# The fix is a declared, config-driven override that:
#   1. BUILDS the dependency from its local source (its own build), then
#   2. packs it into a self-contained tarball (`npm pack` / `pnpm pack`), then
#   3. installs that tarball into the consumer as a real, built package.
#
# Installing a built tarball (not a live symlink) lets the package manager
# dedupe peer dependencies against the consumer's single copy — React resolves
# once, so hooks work.
#
# Declaring overrides
# -------------------
# Overrides are read from `HOMEBOY_SETTINGS_JSON` (the same channel the
# WordPress build uses for `package_artifacts`) under the key
# `local_workspace_dependencies`, an array of objects:
#
#   {
#     "local_workspace_dependencies": [
#       {
#         "name": "@automattic/agenttic-ui",        // required: package name
#         "path": "../agenttic",                     // required: source path,
#                                                    //   relative to the
#                                                    //   consumer (or absolute)
#         "package_dir": "packages/agenttic-ui",     // optional: subdir within
#                                                    //   `path` to pack (monorepo)
#         "build": "pnpm install --frozen-lockfile && pnpm --filter @automattic/agenttic-ui build",
#                                                    // optional: explicit build
#                                                    //   command run in `path`.
#                                                    //   Defaults to auto:
#                                                    //   install + run `build`.
#         "package_manager": "pnpm"                  // optional: override
#                                                    //   auto-detection
#       }
#     ]
#   }
#
# This helper is generic: it carries no WordPress/Studio knowledge. The
# WordPress build sources it; the generic Node.js build runner uses it too.

# Apply all declared local workspace dependency overrides for a consumer.
#
# Usage: homeboy_apply_local_workspace_dependencies <consumer_dir>
#
# Reads HOMEBOY_SETTINGS_JSON. A no-op (returns 0) when no overrides are
# declared, so callers can always invoke it unconditionally. Returns non-zero
# when a declared override is invalid or fails to build/pack/install — a
# misconfigured override is a hard build failure, not a silent skip, because the
# resulting bundle would be broken.
#
# Optional env:
#   HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS  Extra flags for the consumer-side
#       `npm install <tarball>` (e.g. "--no-audit --no-fund --legacy-peer-deps").
#       `--no-save` is always added so the consumer's committed package.json and
#       lockfile are never mutated by the build.
homeboy_apply_local_workspace_dependencies() {
    local consumer_dir="$1"

    if [ -z "$consumer_dir" ]; then
        echo "[local-workspace-deps] consumer directory is required" >&2
        return 1
    fi
    if [ ! -d "$consumer_dir" ]; then
        echo "[local-workspace-deps] consumer directory does not exist: $consumer_dir" >&2
        return 1
    fi

    # No settings at all → nothing declared → no-op.
    case "${HOMEBOY_SETTINGS_JSON:-}" in
        ""|"{}") return 0 ;;
    esac

    if ! command -v node >/dev/null 2>&1; then
        echo "[local-workspace-deps] node is required to parse local_workspace_dependencies" >&2
        return 1
    fi

    local consumer_abs
    consumer_abs="$(cd "$consumer_dir" && pwd)"

    # Emit one tab-separated record per declared override:
    #   name<TAB>path<TAB>package_dir<TAB>package_manager<TAB>base64(build)
    # build is base64-encoded so embedded spaces/newlines survive transport.
    # Validation happens inside node; a non-zero exit aborts the build.
    local records
    if ! records="$(node <<'NODE'
const raw = process.env.HOMEBOY_SETTINGS_JSON || '{}';
let settings;
try {
  settings = JSON.parse(raw);
} catch (e) {
  process.stderr.write('[local-workspace-deps] HOMEBOY_SETTINGS_JSON is not valid JSON.\n');
  process.exit(1);
}
if (settings === null || typeof settings !== 'object' || Array.isArray(settings)) {
  process.stderr.write('[local-workspace-deps] HOMEBOY_SETTINGS_JSON must be a JSON object.\n');
  process.exit(1);
}
const entries = settings.local_workspace_dependencies;
if (entries === undefined) {
  process.exit(0);
}
if (!Array.isArray(entries)) {
  process.stderr.write('[local-workspace-deps] local_workspace_dependencies must be an array.\n');
  process.exit(1);
}
const cleanField = (value, field, index) => {
  if (typeof value !== 'string' || value === '') {
    process.stderr.write(`[local-workspace-deps] entry ${index}: "${field}" must be a non-empty string.\n`);
    process.exit(1);
  }
  if (/[\t\n]/.test(value)) {
    process.stderr.write(`[local-workspace-deps] entry ${index}: "${field}" cannot contain tabs or newlines.\n`);
    process.exit(1);
  }
  return value;
};
const optionalField = (value, field, index) => {
  if (value === undefined || value === null || value === '') return '';
  if (typeof value !== 'string') {
    process.stderr.write(`[local-workspace-deps] entry ${index}: "${field}" must be a string when set.\n`);
    process.exit(1);
  }
  return value;
};
entries.forEach((entry, index) => {
  if (entry === null || typeof entry !== 'object' || Array.isArray(entry)) {
    process.stderr.write(`[local-workspace-deps] entry ${index} must be an object.\n`);
    process.exit(1);
  }
  const name = cleanField(entry.name, 'name', index);
  const sourcePath = cleanField(entry.path, 'path', index);
  const packageDir = optionalField(entry.package_dir, 'package_dir', index);
  const packageManager = optionalField(entry.package_manager, 'package_manager', index);
  if (/[\t\n]/.test(packageDir) || /[\t\n]/.test(packageManager)) {
    process.stderr.write(`[local-workspace-deps] entry ${index}: package_dir/package_manager cannot contain tabs or newlines.\n`);
    process.exit(1);
  }
  if (packageManager && !['npm', 'pnpm', 'yarn'].includes(packageManager)) {
    process.stderr.write(`[local-workspace-deps] entry ${index}: package_manager must be npm, pnpm, or yarn.\n`);
    process.exit(1);
  }
  const build = optionalField(entry.build, 'build', index);
  const buildB64 = Buffer.from(build, 'utf8').toString('base64');
  process.stdout.write([name, sourcePath, packageDir, packageManager, buildB64].join('\t') + '\n');
});
NODE
)"; then
        return 1
    fi

    if [ -z "$records" ]; then
        return 0
    fi

    local name source_path package_dir package_manager build_b64
    while IFS=$'\t' read -r name source_path package_dir package_manager build_b64; do
        [ -n "$name" ] || continue
        if ! _homeboy_local_workspace_dep_one \
            "$consumer_abs" "$name" "$source_path" "$package_dir" "$package_manager" "$build_b64"; then
            return 1
        fi
    done <<EOF
$records
EOF

    return 0
}

# Build + pack + install a single declared override. Internal helper.
_homeboy_local_workspace_dep_one() {
    local consumer_abs="$1"
    local name="$2"
    local source_path="$3"
    local package_dir="$4"
    local package_manager="$5"
    local build_b64="$6"

    echo "[local-workspace-deps] Resolving '$name' from '$source_path'"

    # Resolve source directory relative to the consumer (or accept absolute).
    local source_abs
    case "$source_path" in
        /*) source_abs="$source_path" ;;
        *)  source_abs="$consumer_abs/$source_path" ;;
    esac
    if [ ! -d "$source_abs" ]; then
        echo "[local-workspace-deps] '$name': source directory not found: $source_abs" >&2
        return 1
    fi
    source_abs="$(cd "$source_abs" && pwd)"

    # The directory whose package we pack (monorepo subpackage support).
    local pack_abs="$source_abs"
    if [ -n "$package_dir" ]; then
        pack_abs="$source_abs/$package_dir"
    fi
    if [ ! -f "$pack_abs/package.json" ]; then
        echo "[local-workspace-deps] '$name': no package.json found at: $pack_abs" >&2
        return 1
    fi
    pack_abs="$(cd "$pack_abs" && pwd)"

    # Validate the package name matches what was declared, so a misconfigured
    # path can't silently install the wrong package.
    local packed_name
    packed_name="$(HOMEBOY_LWD_PKG="$pack_abs/package.json" node -e 'process.stdout.write(String(require(process.env.HOMEBOY_LWD_PKG).name || ""))' 2>/dev/null || true)"
    if [ "$packed_name" != "$name" ]; then
        echo "[local-workspace-deps] '$name': package at $pack_abs is named '${packed_name:-<none>}', expected '$name'" >&2
        return 1
    fi

    # Detect the dependency's package manager unless overridden.
    local pm="$package_manager"
    if [ -z "$pm" ]; then
        if [ -f "$source_abs/pnpm-lock.yaml" ]; then
            pm="pnpm"
        elif [ -f "$source_abs/yarn.lock" ]; then
            pm="yarn"
        else
            pm="npm"
        fi
    fi
    if ! command -v "$pm" >/dev/null 2>&1; then
        echo "[local-workspace-deps] '$name': package manager '$pm' is not available" >&2
        return 1
    fi

    # Build the dependency from source.
    local build_cmd=""
    if [ -n "$build_b64" ]; then
        build_cmd="$(printf '%s' "$build_b64" | base64 --decode 2>/dev/null || printf '%s' "$build_b64" | base64 -d)"
    fi

    if [ -n "$build_cmd" ]; then
        echo "[local-workspace-deps] '$name': building via declared command"
        if ! ( cd "$source_abs" && bash -c "$build_cmd" ); then
            echo "[local-workspace-deps] '$name': declared build command failed" >&2
            return 1
        fi
    else
        # Install where dependencies are actually declared: the source root for
        # a monorepo (so workspaces resolve), otherwise the packed package dir.
        local install_dir="$source_abs"
        if [ ! -f "$source_abs/package.json" ]; then
            install_dir="$pack_abs"
        fi
        echo "[local-workspace-deps] '$name': installing dependency sources ($pm) in $install_dir"
        if ! _homeboy_local_workspace_dep_install "$pm" "$install_dir"; then
            echo "[local-workspace-deps] '$name': dependency install failed" >&2
            return 1
        fi
        # Prefer a build script in the packed package; fall back to source root.
        local build_dir=""
        if _homeboy_local_workspace_dep_has_build_script "$pack_abs"; then
            build_dir="$pack_abs"
        elif _homeboy_local_workspace_dep_has_build_script "$source_abs"; then
            build_dir="$source_abs"
        else
            echo "[local-workspace-deps] '$name': no build command declared and no 'build' script found in $pack_abs or $source_abs" >&2
            return 1
        fi
        echo "[local-workspace-deps] '$name': running build script in $build_dir"
        if ! ( cd "$build_dir" && _homeboy_local_workspace_dep_run_build "$pm" ); then
            echo "[local-workspace-deps] '$name': build script failed" >&2
            return 1
        fi
    fi

    # Pack the built package into a self-contained tarball.
    local pack_dest
    pack_dest="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-lwd.XXXXXX")"
    echo "[local-workspace-deps] '$name': packing built artifact"
    if ! ( cd "$pack_abs" && _homeboy_local_workspace_dep_pack "$pm" "$pack_dest" ); then
        echo "[local-workspace-deps] '$name': pack failed" >&2
        rm -rf "$pack_dest"
        return 1
    fi

    local tarball
    tarball="$(find "$pack_dest" -maxdepth 1 -name '*.tgz' -type f 2>/dev/null | head -1)"
    if [ -z "$tarball" ] || [ ! -f "$tarball" ]; then
        echo "[local-workspace-deps] '$name': no tarball produced in $pack_dest" >&2
        rm -rf "$pack_dest"
        return 1
    fi

    # Install the built tarball into the consumer as a real package. Peer deps
    # (React, etc.) dedupe against the consumer's already-installed copy, which
    # is the whole point — a live symlink would not dedupe.
    local install_flags="--no-save ${HOMEBOY_LOCAL_WORKSPACE_DEP_NPM_FLAGS:---no-audit --no-fund}"
    echo "[local-workspace-deps] '$name': installing built tarball into consumer (npm install $install_flags)"
    if ! ( cd "$consumer_abs" && npm install "$tarball" $install_flags ); then
        echo "[local-workspace-deps] '$name': consumer install of built tarball failed" >&2
        rm -rf "$pack_dest"
        return 1
    fi

    # Structured evidence line (mirrors the package_artifacts manifest style).
    local sha256=""
    if command -v shasum >/dev/null 2>&1; then
        sha256="$(shasum -a 256 "$tarball" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256="$(sha256sum "$tarball" | awk '{print $1}')"
    fi
    printf '{"type":"nodejs.local_workspace_dependency","name":"%s","source":"%s","pack_dir":"%s","tarball":"%s","sha256":"%s"}\n' \
        "$name" "$source_abs" "$pack_abs" "$(basename "$tarball")" "$sha256"

    rm -rf "$pack_dest"
    return 0
}

_homeboy_local_workspace_dep_install() {
    local pm="$1"
    local dir="$2"
    case "$pm" in
        pnpm)
            if [ -f "$dir/pnpm-lock.yaml" ]; then
                ( cd "$dir" && pnpm install --frozen-lockfile )
            else
                ( cd "$dir" && pnpm install )
            fi
            ;;
        yarn)
            ( cd "$dir" && yarn install --frozen-lockfile )
            ;;
        npm|*)
            if [ -f "$dir/package-lock.json" ]; then
                ( cd "$dir" && npm ci --no-audit --no-fund )
            else
                ( cd "$dir" && npm install --no-audit --no-fund )
            fi
            ;;
    esac
}

_homeboy_local_workspace_dep_has_build_script() {
    local dir="$1"
    [ -f "$dir/package.json" ] || return 1
    HOMEBOY_LWD_PKG="$dir/package.json" node -e '
        const pkg = require(process.env.HOMEBOY_LWD_PKG);
        process.exit(pkg.scripts && pkg.scripts.build ? 0 : 1);
    ' 2>/dev/null
}

_homeboy_local_workspace_dep_run_build() {
    local pm="$1"
    case "$pm" in
        pnpm) pnpm run build ;;
        yarn) yarn run build ;;
        npm|*) npm run build ;;
    esac
}

_homeboy_local_workspace_dep_pack() {
    local pm="$1"
    local dest="$2"
    case "$pm" in
        pnpm) pnpm pack --pack-destination "$dest" ;;
        # yarn classic writes to the cwd with --filename; pack there then move.
        yarn)
            yarn pack --filename "$dest/package.tgz"
            ;;
        npm|*) npm pack --pack-destination "$dest" ;;
    esac
}
