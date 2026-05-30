#!/usr/bin/env bash
set -euo pipefail

# Emit the WordPress -> browser-runner handoff file for bench workflows.
#
# The WordPress extension owns WordPress setup and target description. Browser
# automation itself belongs to the Node extension/Playwright helper, which can
# consume ${HOMEBOY_BENCH_SHARED_STATE}/browser-target.json after this runner
# prepares it.

homeboy_wordpress_browser_target_enabled() {
    local settings_json="$1"

    if [ -z "$settings_json" ] || [ "$settings_json" = "{}" ]; then
        return 1
    fi

    printf '%s' "$settings_json" | jq -e '
        .bench_browser_target as $target
        | ($target == true) or (($target | type) == "object" and ($target.enabled // false) == true)
    ' >/dev/null 2>&1
}

homeboy_wordpress_emit_browser_target() {
    local settings_json="$1"
    local shared_state_host="$2"
    local component_id="$3"
    local plugin_slug="$4"
    local runtime_mode="$5"

    if ! homeboy_wordpress_browser_target_enabled "$settings_json"; then
        return 0
    fi

    if [ -z "$shared_state_host" ]; then
        echo "Error: bench_browser_target.enabled requires HOMEBOY_BENCH_SHARED_STATE so browser-target.json has a stable handoff directory." >&2
        return 1
    fi

    mkdir -p "$shared_state_host"

    local target_path="${shared_state_host}/browser-target.json"
    local tmp_path="${target_path}.tmp"

    # Resolve optional secret indirection without logging the value. The target
    # file is a handoff artifact, not a report artifact; callers must redact it
    # before publishing bench outputs.
    local password=""
    local password_env=""
    password_env=$(printf '%s' "$settings_json" | jq -r '
        .bench_browser_target
        | if type == "object" then (.login.password_env // .login.passwordEnv // "") else "" end
    ' 2>/dev/null || true)
    if [ -n "$password_env" ]; then
        if [[ ! "$password_env" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
            echo "Error: bench_browser_target.login.password_env must be a valid environment variable name." >&2
            return 1
        fi
        if [ -z "${!password_env+x}" ]; then
            echo "Error: bench_browser_target.login.password_env references unset environment variable '${password_env}'." >&2
            return 1
        fi
        password="${!password_env}"
    fi

    printf '%s' "$settings_json" | jq \
        --arg component_id "$component_id" \
        --arg plugin_slug "$plugin_slug" \
        --arg runtime_mode "$runtime_mode" \
        --arg password "$password" '
        def target_config:
            if .bench_browser_target == true then {} else (.bench_browser_target // {}) end;

        (target_config) as $target
        | ($target.baseUrl // $target.base_url // "") as $base_url
        | ($target.adminUrl // $target.admin_url // (if $base_url != "" then ($base_url | sub("/*$"; "/")) + "wp-admin/" else "" end)) as $admin_url
        | ($target.login // {"method": "none"}) as $login_raw
        | {
            schemaVersion: 1,
            kind: "wordpress",
            lifecycle: {
                server: (if $base_url == "" then "not_started" else "external" end),
                keepAlive: (if $base_url == "" then "none" else "caller" end),
                note: (if $base_url == "" then "The WP Codebox bench command does not keep an HTTP server alive after bench execution; provide baseUrl for an already-running target or use this as metadata only." else "The browser helper connects to the provided URL; the caller owns server lifetime." end)
            },
            baseUrl: $base_url,
            adminUrl: $admin_url,
            login: (
                if (($login_raw | type) == "object") then
                    ($login_raw
                        | if (.password_env // .passwordEnv // "") != "" and $password != "" then . + {password: $password} else . end
                        | del(.password_env, .passwordEnv))
                else
                    {"method": "none"}
                end
            ),
            metadata: {
                wpVersion: "6.9",
                componentId: $component_id,
                pluginSlug: $plugin_slug,
                runtimeMode: $runtime_mode
            },
            artifactPolicy: {
                publishRaw: false,
                secretFields: ["login.password", "login.url"]
            }
        }
    ' > "$tmp_path"

    mv "$tmp_path" "$target_path"
    echo "Browser bench target: ${target_path}"
}
