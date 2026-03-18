#!/bin/bash
# Sweatpants Bridge - Generic bridge to Sweatpants automation engine
# No project-specific logic - works with any Sweatpants instance

set -e

# Configure bridge framework
BRIDGE_NAME="sweatpants"
BRIDGE_ENTITY="endpoint"
BRIDGE_URL_FIELD="url"
BRIDGE_AUTH_FIELD="auth"
BRIDGE_AUTH_HEADER="Authorization: Bearer"
BRIDGE_DEFAULT_ID="local"
BRIDGE_DEFAULT_CONFIG='{
  "local": {
    "url": "http://127.0.0.1:8420",
    "auth": null
  }
}'

# Source shared bridge framework
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRAMEWORK="${SCRIPT_DIR}/../../lib/bridge-framework.sh"
if [[ ! -f "$FRAMEWORK" ]]; then
    echo "Error: bridge-framework.sh not found at $FRAMEWORK" >&2
    exit 1
fi
source "$FRAMEWORK"

# --- Sweatpants-specific commands ---

cmd_status() {
    local endpoint_url="$1"
    local auth="$2"

    local response
    response=$(bridge_api_request GET "$endpoint_url" "/api/status" "" "$auth")

    if [[ $? -ne 0 ]]; then
        echo -e "${RED}Failed to connect to Sweatpants at $endpoint_url${NC}"
        exit 1
    fi

    echo -e "${BLUE}Sweatpants Status${NC} ($endpoint_url)"
    echo "=================="
    echo "$response" | jq .
}

cmd_extension_list() {
    local endpoint_url="$1"
    local auth="$2"

    local response
    response=$(bridge_api_request GET "$endpoint_url" "/api/extensions" "" "$auth")

    echo -e "${BLUE}Available Extensions${NC}"
    echo "================="
    echo "$response" | jq -r '.extensions[] | "  \(.id): \(.description // "No description")"'
}

cmd_run() {
    local endpoint_url="$1"
    local auth="$2"
    shift 2

    local extension_id=""
    local inputs="{}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            -i|--input)
                local key_value="$2"
                local key="${key_value%%=*}"
                local value="${key_value#*=}"
                inputs=$(echo "$inputs" | jq --arg k "$key" --arg v "$value" '. + {($k): $v}')
                shift 2
                ;;
            *)
                if [[ -z "$extension_id" ]]; then
                    extension_id="$1"
                fi
                shift
                ;;
        esac
    done

    if [[ -z "$extension_id" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants run <extension-id> [-i key=value]...${NC}"
        exit 1
    fi

    local payload
    payload=$(jq -n --arg extension "$extension_id" --argjson inputs "$inputs" \
        '{"extension": $extension, "inputs": $inputs}')

    echo -e "${BLUE}Starting job: $extension_id${NC}"

    local response
    response=$(bridge_api_request POST "$endpoint_url" "/api/jobs" "$payload" "$auth")

    local job_id
    job_id=$(echo "$response" | jq -r '.job_id // .id // empty')

    if [[ -n "$job_id" ]]; then
        echo -e "${GREEN}Job started: $job_id${NC}"
        echo "View logs: homeboy sweatpants logs $job_id --follow"
    else
        echo -e "${YELLOW}Response:${NC}"
        echo "$response" | jq .
    fi
}

cmd_logs() {
    local endpoint_url="$1"
    local auth="$2"
    local job_id="$3"
    local follow="${4:-}"

    if [[ -z "$job_id" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants logs <job-id> [--follow]${NC}"
        exit 1
    fi

    if [[ "$follow" == "--follow" || "$follow" == "-f" ]]; then
        echo -e "${YELLOW}Following logs (polling mode)...${NC}"
        echo "Press Ctrl+C to stop"
        echo ""

        local last_offset=0
        while true; do
            local response
            response=$(bridge_api_request GET "$endpoint_url" "/api/jobs/$job_id/logs?offset=$last_offset" "" "$auth")

            local logs
            logs=$(echo "$response" | jq -r '.logs // empty')

            if [[ -n "$logs" ]]; then
                echo "$logs"
                last_offset=$(echo "$response" | jq -r '.offset // 0')
            fi

            local status
            status=$(echo "$response" | jq -r '.status // empty')

            if [[ "$status" == "completed" || "$status" == "failed" || "$status" == "cancelled" ]]; then
                echo ""
                echo -e "${BLUE}Job $status${NC}"
                break
            fi

            sleep 2
        done
    else
        local response
        response=$(bridge_api_request GET "$endpoint_url" "/api/jobs/$job_id/logs" "" "$auth")
        echo "$response" | jq -r '.logs // .'
    fi
}

cmd_jobs() {
    local endpoint_url="$1"
    local auth="$2"

    local response
    response=$(bridge_api_request GET "$endpoint_url" "/api/jobs" "" "$auth")

    echo -e "${BLUE}Jobs${NC}"
    echo "====="
    echo "$response" | jq -r '.jobs[] | "\(.id) [\(.status)]: \(.extension) - \(.created_at // "unknown")"'
}

cmd_cancel() {
    local endpoint_url="$1"
    local auth="$2"
    local job_id="$3"

    if [[ -z "$job_id" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants cancel <job-id>${NC}"
        exit 1
    fi

    local response
    response=$(bridge_api_request POST "$endpoint_url" "/api/jobs/$job_id/cancel" "" "$auth")

    echo -e "${GREEN}Cancelled job: $job_id${NC}"
}

show_help() {
    echo "Sweatpants Bridge - Homeboy extension for Sweatpants automation engine"
    echo ""
    echo "Usage: homeboy sweatpants [endpoint] <command> [args...]"
    echo ""
    echo "Endpoint Management:"
    echo "  endpoints                    List configured endpoints"
    echo "  endpoint add <id> <url>      Add new endpoint"
    echo "  endpoint remove <id>         Remove endpoint"
    echo ""
    echo "Commands (use with optional endpoint prefix):"
    echo "  status                       Show Sweatpants status"
    echo "  extension list               List available extensions"
    echo "  run <extension> [-i k=v]...  Run a extension with inputs"
    echo "  jobs                         List jobs"
    echo "  logs <job-id> [--follow]     View job logs"
    echo "  cancel <job-id>              Cancel a running job"
    echo ""
    echo "Examples:"
    echo "  homeboy sweatpants status"
    echo "  homeboy sweatpants local status"
    echo "  homeboy sweatpants run my-extension -i key=value"
    echo "  homeboy sweatpants vps run scraper -i tags=lo-fi"
    echo "  homeboy sweatpants logs abc123 --follow"
}

# --- Main entry point ---

main() {
    if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" || "$1" == "help" ]]; then
        show_help
        exit 0
    fi

    bridge_ensure_config

    # Handle entity management commands
    case "$1" in
        endpoints)
            bridge_cmd_list
            exit 0
            ;;
        endpoint)
            case "$2" in
                add)    bridge_cmd_add "$3" "$4" "$5"; exit 0 ;;
                remove) bridge_cmd_remove "$3"; exit 0 ;;
                *)      echo -e "${RED}Unknown endpoint command: $2${NC}"; exit 1 ;;
            esac
            ;;
    esac

    # Resolve endpoint from args
    local args=("$@")
    bridge_resolve_entity args
    set -- "${args[@]}"

    local cmd="$1"
    shift

    # Route to command handler
    case "$cmd" in
        status)     cmd_status "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" ;;
        extension)
            case "${1:-}" in
                list) cmd_extension_list "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" ;;
                *)    echo -e "${RED}Unknown extension command: ${1:-}${NC}"; exit 1 ;;
            esac
            ;;
        run)        cmd_run "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "$@" ;;
        jobs)       cmd_jobs "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" ;;
        logs)       cmd_logs "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "$@" ;;
        cancel)     cmd_cancel "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "$@" ;;
        *)          echo -e "${RED}Unknown command: $cmd${NC}"; show_help; exit 1 ;;
    esac
}

main "$@"
