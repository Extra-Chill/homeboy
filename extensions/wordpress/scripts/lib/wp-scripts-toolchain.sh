#!/usr/bin/env bash

# Resolve the @wordpress/scripts CLI a WordPress component's npm scripts
# invoke (`wp-scripts build`, `wp-scripts lint-js`, `wp-scripts test-unit-js`,
# `wp-scripts lint-style`, ...) the same way the extension already resolves
# phpcs: the component's own npm-installed copy first, falling back to the
# extension's own pinned copy only when the component has none
# (homeboy-extensions#2870).
#
# A component that vendors its own @wordpress/scripts keeps building exactly
# as it does today: npm's own script resolution always prefers
# `node_modules/.bin` over anything on $PATH (verified empirically -- npm
# constructs the child process PATH from the component's own ancestor
# node_modules/.bin dirs first, the inherited PATH last), so this function is
# a pure no-op whenever `<project_dir>/node_modules/.bin/wp-scripts` already
# exists. That is what makes it safe to call unconditionally for every
# wordpress-scripts build: it never overrides a real local install, it only
# fills the gap when a consumer drops the dependency and starts relying on
# the extension. No flag day -- a component opts in by removing its own
# `@wordpress/scripts` devDependency, nothing else changes underneath it
# until then.
#
# Exports, only when the extension toolchain is actually applied:
#
#   PATH
#       Extension's node_modules/.bin first, so `npm run <script>` (build,
#       lint:js, lint:css, test:js, ... whatever the component names its
#       script) resolves the bare `wp-scripts` command it invokes to the
#       extension's copy instead of failing with "command not found".
#
#   NODE_PATH
#       The toolchain's node_modules, so any
#       `require('@wordpress/scripts/...')` or `require('@wordpress/<package>')`
#       bare specifier resolves once the component's own node_modules walk
#       comes up empty. Node consults NODE_PATH last, after the ordinary
#       node_modules walk starting from the requiring file (verified), so
#       this can never shadow a package the component genuinely has
#       installed -- it is a pure fallback.
#
#   HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG
#       Absolute path to the toolchain's
#       `@wordpress/scripts/config/webpack.config.js`. This is the documented
#       consumer contract for a custom `webpack.config.js` that extends the
#       default config -- see README.md's "wp-scripts (JavaScript build
#       toolchain)" section. A consumer dropping its own `@wordpress/scripts`
#       changes:
#
#           const defaultConfig = require( '@wordpress/scripts/config/webpack.config' );
#
#       to:
#
#           const defaultConfig = require( process.env.HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG || '@wordpress/scripts/config/webpack.config' );
#
#       NODE_PATH resolves the unmodified bare specifier too (verified), so a
#       custom config that has not made this one-line change yet still
#       builds. The env var is the explicit, greppable primary contract the
#       issue asked for; NODE_PATH is defense in depth, not the documented
#       path a consumer is expected to take.
#
# The extension's pinned @wordpress/scripts lives in its own isolated npm
# project at `<extension_path>/toolchains/wp-scripts/`, NOT in
# `<extension_path>/node_modules` alongside the extension's own lint tooling
# (wordpress/package.json pins `eslint` 9.x + `@wordpress/eslint-plugin` for
# `homeboy lint`'s flat-config ESLint runner; @wordpress/scripts pins its own
# `eslint` 8.x for classic-config `wp-scripts lint-js`). A single shared
# node_modules tree hoists one `eslint` to the top level; `wp-scripts
# lint-js` resolves `eslint` via the `resolve-bin` package, which is itself
# hoisted, so it resolves `eslint` from ITS OWN hoisted location rather than
# `@wordpress/scripts`'s nested copy -- verified directly while building this
# extension (`resolve-bin` found the extension's flat-config eslint 9.x and
# `wp-scripts lint-js` failed with "You're using eslint.config.js, some
# command line flags are no longer available"). Isolating the install keeps
# @wordpress/scripts' entire dependency tree (webpack, babel, eslint,
# stylelint, jest, ...) self-contained, exactly like `wordpress/composer.json`
# keeps the PHP toolchain (phpcs, phpstan, wp-cli) out of any consumer's own
# dependency tree.
#
# Usage: homeboy_wordpress_apply_extension_wp_scripts_toolchain <project_dir> <extension_path>
homeboy_wordpress_apply_extension_wp_scripts_toolchain() {
    local project_dir="$1"
    local extension_path="$2"
    local toolchain_dir="${extension_path}/toolchains/wp-scripts"

    [ -x "${project_dir}/node_modules/.bin/wp-scripts" ] && return 0

    local extension_wp_scripts="${toolchain_dir}/node_modules/.bin/wp-scripts"
    [ -x "$extension_wp_scripts" ] || return 0

    echo "No local @wordpress/scripts found in ${project_dir}; using the wordpress extension's pinned copy (${extension_wp_scripts})"

    export PATH="${toolchain_dir}/node_modules/.bin:${PATH}"
    export NODE_PATH="${toolchain_dir}/node_modules${NODE_PATH:+:${NODE_PATH}}"
    export HOMEBOY_WP_SCRIPTS_WEBPACK_CONFIG="${toolchain_dir}/node_modules/@wordpress/scripts/config/webpack.config.js"
}

# True when the given package.json declares @wordpress/scripts as a real
# dependency (any of dependencies/devDependencies/peerDependencies), as
# opposed to merely invoking the `wp-scripts` CLI from an npm script without
# vendoring it. Distinguishes "this component owns the toolchain" (expect and
# require a local node_modules/.bin/wp-scripts) from "this component expects
# the wordpress extension to supply it".
homeboy_wordpress_package_declares_wp_scripts() {
    local package_json="${1:-package.json}"
    [ -f "$package_json" ] || return 1
    command -v node >/dev/null 2>&1 || return 1

    HOMEBOY_WP_SCRIPTS_PACKAGE_JSON="$package_json" node -e '
        const fs = require("fs");
        let pkg;
        try {
            pkg = JSON.parse(fs.readFileSync(process.env.HOMEBOY_WP_SCRIPTS_PACKAGE_JSON, "utf8"));
        } catch {
            process.exit(1);
        }
        const deps = {
            ...(pkg.dependencies || {}),
            ...(pkg.devDependencies || {}),
            ...(pkg.peerDependencies || {}),
        };
        process.exit(deps["@wordpress/scripts"] ? 0 : 1);
    '
}

# True when any npm script in the given package.json invokes the `wp-scripts`
# CLI, whether or not the package itself declares @wordpress/scripts as a
# dependency. This is the "extension-supplied toolchain" detection: a
# component can call `wp-scripts build` (or lint-js, lint-style,
# test-unit-js, ...) from its own package.json scripts without vendoring the
# package locally, and still be recognized as a wordpress-scripts project.
homeboy_wordpress_package_scripts_invoke_wp_scripts() {
    local package_json="${1:-package.json}"
    [ -f "$package_json" ] || return 1
    command -v node >/dev/null 2>&1 || return 1

    HOMEBOY_WP_SCRIPTS_PACKAGE_JSON="$package_json" node -e '
        const fs = require("fs");
        let pkg;
        try {
            pkg = JSON.parse(fs.readFileSync(process.env.HOMEBOY_WP_SCRIPTS_PACKAGE_JSON, "utf8"));
        } catch {
            process.exit(1);
        }
        const scripts = pkg.scripts || {};
        const invoked = Object.values(scripts).some(
            (command) => /(^|[\s&|;])wp-scripts(\s|$)/.test(String(command))
        );
        process.exit(invoked ? 0 : 1);
    '
}
