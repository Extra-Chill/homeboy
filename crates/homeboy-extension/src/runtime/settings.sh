#!/usr/bin/env bash

# Safe accessors for Homeboy's merged extension settings payload.

homeboy_settings_json() {
    local settings_json="${HOMEBOY_SETTINGS_JSON:-}"
    if [ -z "$settings_json" ]; then
        printf '{}'
        return 0
    fi
    if printf '%s' "$settings_json" | jq -e 'type == "object"' >/dev/null 2>&1; then
        printf '%s' "$settings_json"
    else
        printf '{}'
    fi
}

homeboy_setting_json() {
    local setting_name="${1:?setting name is required}"
    local default_json="${2:-null}"
    local expression="${3:-.$setting_name}"
    printf '%s' "$(homeboy_settings_json)" | jq -c "$expression" 2>/dev/null || printf '%s' "$default_json"
}

homeboy_setting() {
    local setting_name="${1:?setting name is required}"
    local expression="${2:-.$setting_name}"
    local default_value="${3:-}"
    local value
    value=$(printf '%s' "$(homeboy_settings_json)" | jq -r "$expression" 2>/dev/null || true)
    case "$value" in ""|null) printf '%s' "$default_value" ;; *) printf '%s' "$value" ;; esac
}

homeboy_setting_bool() {
    local setting_name="${1:?setting name is required}"
    local default_value="${2:-false}"
    local expression="${3:-.$setting_name}"
    local value
    value=$(printf '%s' "$(homeboy_settings_json)" | jq -r "$expression" 2>/dev/null || true)
    case "$value" in true|TRUE|1|yes|YES|on|ON) printf 'true' ;; false|FALSE|0|no|NO|off|OFF) printf 'false' ;; *) case "$default_value" in true|TRUE|1|yes|YES|on|ON) printf 'true' ;; *) printf 'false' ;; esac ;; esac
}

homeboy_setting_array() {
    local setting_name="${1:?setting name is required}"
    local expression="${2:-.$setting_name}"
    local default_json="${3:-[]}"
    local value
    value=$(printf '%s' "$(homeboy_settings_json)" | jq -c "$expression" 2>/dev/null || true)
    if printf '%s' "$value" | jq -e 'type == "array"' >/dev/null 2>&1; then printf '%s' "$value"; else printf '%s' "$default_json"; fi
}
