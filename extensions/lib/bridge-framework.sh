#!/usr/bin/env bash
#
# Shared bridge framework for homeboy extensions that wrap remote APIs.
#
# Provides: config directory management, endpoint CRUD, API request helper,
# color constants, and endpoint resolution utilities.
#
# Usage:
#   BRIDGE_NAME="sweatpants"
#   BRIDGE_ENTITY="endpoint"       # or "router", "server", etc.
#   BRIDGE_URL_FIELD="url"         # JSON field for the URL (default: "url")
#   BRIDGE_AUTH_FIELD="auth"       # JSON field for the auth token (default: "auth")
#   BRIDGE_AUTH_HEADER="Authorization: Bearer"  # or "X-API-Key:" etc.
#   BRIDGE_DEFAULT_ID="local"
#   BRIDGE_DEFAULT_CONFIG='{...}'  # Default JSON for the config file
#
#   source "path/to/bridge-framework.sh"
#
# After sourcing, these functions are available:
#   bridge_ensure_config
#   bridge_get_url <id>
#   bridge_get_auth <id>
#   bridge_is_entity <name>
#   bridge_api_request <method> <url> <path> <data> <auth>
#   bridge_cmd_list
#   bridge_cmd_add <id> <url> [extra_fields...]
#   bridge_cmd_remove <id>
#   bridge_resolve_entity_and_shift  (sets BRIDGE_RESOLVED_URL and BRIDGE_RESOLVED_AUTH)

# Color constants
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Defaults
BRIDGE_NAME="${BRIDGE_NAME:-bridge}"
BRIDGE_ENTITY="${BRIDGE_ENTITY:-endpoint}"
BRIDGE_URL_FIELD="${BRIDGE_URL_FIELD:-url}"
BRIDGE_AUTH_FIELD="${BRIDGE_AUTH_FIELD:-auth}"
BRIDGE_AUTH_HEADER="${BRIDGE_AUTH_HEADER:-Authorization: Bearer}"
BRIDGE_DEFAULT_ID="${BRIDGE_DEFAULT_ID:-local}"

# Derived
BRIDGE_CONFIG_DIR="$HOME/.config/homeboy/${BRIDGE_NAME}"
BRIDGE_CONFIG_FILE="${BRIDGE_CONFIG_DIR}/${BRIDGE_ENTITY}s.json"

# Ensure config directory and file exist
bridge_ensure_config() {
    if [[ ! -d "$BRIDGE_CONFIG_DIR" ]]; then
        mkdir -p "$BRIDGE_CONFIG_DIR"
    fi

    if [[ ! -f "$BRIDGE_CONFIG_FILE" ]]; then
        if [[ -n "${BRIDGE_DEFAULT_CONFIG:-}" ]]; then
            echo "$BRIDGE_DEFAULT_CONFIG" > "$BRIDGE_CONFIG_FILE"
        else
            echo '{}' > "$BRIDGE_CONFIG_FILE"
        fi
    fi
}

# Get URL for an entity
bridge_get_url() {
    local entity_id="$1"
    bridge_ensure_config

    if ! command -v jq &> /dev/null; then
        echo -e "${RED}Error: jq is required but not installed${NC}" >&2
        exit 1
    fi

    local url
    url=$(jq -r --arg id "$entity_id" --arg field "$BRIDGE_URL_FIELD" \
        '.[$id][$field] // empty' "$BRIDGE_CONFIG_FILE")

    if [[ -z "$url" ]]; then
        echo -e "${RED}Error: ${BRIDGE_ENTITY} '$entity_id' not found${NC}" >&2
        echo -e "Available ${BRIDGE_ENTITY}s:" >&2
        jq -r 'keys[]' "$BRIDGE_CONFIG_FILE" >&2
        exit 1
    fi

    echo "$url"
}

# Get auth token for an entity
bridge_get_auth() {
    local entity_id="$1"
    jq -r --arg id "$entity_id" --arg field "$BRIDGE_AUTH_FIELD" \
        '.[$id][$field] // empty' "$BRIDGE_CONFIG_FILE"
}

# Check if a name is a configured entity
bridge_is_entity() {
    local name="$1"
    bridge_ensure_config
    jq -e --arg id "$name" 'has($id)' "$BRIDGE_CONFIG_FILE" > /dev/null 2>&1
}

# Make API request
bridge_api_request() {
    local method="$1"
    local base_url="$2"
    local path="$3"
    local data="$4"
    local auth="$5"

    local curl_args=(-s -X "$method")

    if [[ -n "$auth" && "$auth" != "null" ]]; then
        curl_args+=(-H "${BRIDGE_AUTH_HEADER} $auth")
    fi

    curl_args+=(-H "Content-Type: application/json")

    if [[ -n "$data" ]]; then
        curl_args+=(-d "$data")
    fi

    curl "${curl_args[@]}" "${base_url}${path}" 2>/dev/null
}

# List configured entities
bridge_cmd_list() {
    bridge_ensure_config
    local entity_upper
    entity_upper="$(echo "${BRIDGE_ENTITY}" | sed 's/.*/\u&/')s"
    echo -e "${BLUE}Configured ${entity_upper}${NC}"
    echo "================================"
    jq -r --arg field "$BRIDGE_URL_FIELD" \
        'to_entries[] | "\(.key): \(.value[$field])"' "$BRIDGE_CONFIG_FILE"
}

# Add a new entity (basic: id + url + auth)
bridge_cmd_add() {
    local id="$1"
    local url="$2"
    local auth="${3:-}"

    if [[ -z "$id" || -z "$url" ]]; then
        echo -e "${RED}Usage: homeboy ${BRIDGE_NAME} ${BRIDGE_ENTITY} add <id> <url> [auth]${NC}"
        exit 1
    fi

    bridge_ensure_config

    local auth_json="null"
    if [[ -n "$auth" ]]; then
        auth_json="\"$auth\""
    fi

    local tmp_file
    tmp_file=$(mktemp)
    jq --arg id "$id" --arg url "$url" --argjson auth "$auth_json" \
        --arg url_field "$BRIDGE_URL_FIELD" --arg auth_field "$BRIDGE_AUTH_FIELD" \
        '.[$id] = {($url_field): $url, ($auth_field): $auth}' "$BRIDGE_CONFIG_FILE" > "$tmp_file"
    mv "$tmp_file" "$BRIDGE_CONFIG_FILE"

    echo -e "${GREEN}Added ${BRIDGE_ENTITY} '$id' -> $url${NC}"
}

# Remove an entity
bridge_cmd_remove() {
    local id="$1"

    if [[ -z "$id" ]]; then
        echo -e "${RED}Usage: homeboy ${BRIDGE_NAME} ${BRIDGE_ENTITY} remove <id>${NC}"
        exit 1
    fi

    bridge_ensure_config

    local tmp_file
    tmp_file=$(mktemp)
    jq --arg id "$id" 'del(.[$id])' "$BRIDGE_CONFIG_FILE" > "$tmp_file"
    mv "$tmp_file" "$BRIDGE_CONFIG_FILE"

    echo -e "${GREEN}Removed ${BRIDGE_ENTITY} '$id'${NC}"
}

# Resolve entity from args and set BRIDGE_RESOLVED_URL / BRIDGE_RESOLVED_AUTH.
# Call before dispatching commands. Consumes the entity arg if present.
# Usage:
#   BRIDGE_ARGS=("$@")
#   bridge_resolve_entity BRIDGE_ARGS
#   set -- "${BRIDGE_ARGS[@]}"
bridge_resolve_entity() {
    local -n _args="$1"
    local entity_id="$BRIDGE_DEFAULT_ID"

    if [[ ${#_args[@]} -gt 0 ]] && bridge_is_entity "${_args[0]}"; then
        entity_id="${_args[0]}"
        _args=("${_args[@]:1}")
    fi

    BRIDGE_RESOLVED_URL=$(bridge_get_url "$entity_id")
    BRIDGE_RESOLVED_AUTH=$(bridge_get_auth "$entity_id")
}
