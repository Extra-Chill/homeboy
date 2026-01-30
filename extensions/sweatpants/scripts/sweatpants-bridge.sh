#!/bin/bash
# Sweatpants Bridge - Generic bridge to Sweatpants automation engine
# No project-specific logic - works with any Sweatpants instance

set -e

CONFIG_DIR="$HOME/.config/homeboy/sweatpants"
ENDPOINTS_FILE="$CONFIG_DIR/endpoints.json"
DEFAULT_ENDPOINT="local"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Ensure config directory exists
ensure_config() {
    if [[ ! -d "$CONFIG_DIR" ]]; then
        mkdir -p "$CONFIG_DIR"
    fi

    if [[ ! -f "$ENDPOINTS_FILE" ]]; then
        echo '{
  "local": {
    "url": "http://127.0.0.1:8420",
    "auth": null
  }
}' > "$ENDPOINTS_FILE"
    fi
}

# Get endpoint URL from config
get_endpoint_url() {
    local endpoint_id="$1"
    ensure_config

    if ! command -v jq &> /dev/null; then
        echo -e "${RED}Error: jq is required but not installed${NC}" >&2
        exit 1
    fi

    local url
    url=$(jq -r --arg id "$endpoint_id" '.[$id].url // empty' "$ENDPOINTS_FILE")

    if [[ -z "$url" ]]; then
        echo -e "${RED}Error: Endpoint '$endpoint_id' not found${NC}" >&2
        echo -e "Available endpoints:" >&2
        jq -r 'keys[]' "$ENDPOINTS_FILE" >&2
        exit 1
    fi

    echo "$url"
}

# Get endpoint auth token from config
get_endpoint_auth() {
    local endpoint_id="$1"
    jq -r --arg id "$endpoint_id" '.[$id].auth // empty' "$ENDPOINTS_FILE"
}

# Make API request to Sweatpants
api_request() {
    local method="$1"
    local endpoint_url="$2"
    local path="$3"
    local data="$4"
    local auth="$5"

    local curl_args=(-s -X "$method")

    if [[ -n "$auth" && "$auth" != "null" ]]; then
        curl_args+=(-H "Authorization: Bearer $auth")
    fi

    curl_args+=(-H "Content-Type: application/json")

    if [[ -n "$data" ]]; then
        curl_args+=(-d "$data")
    fi

    curl "${curl_args[@]}" "${endpoint_url}${path}"
}

# List configured endpoints
cmd_endpoints() {
    ensure_config
    echo -e "${BLUE}Configured Sweatpants Endpoints${NC}"
    echo "================================"
    jq -r 'to_entries[] | "\(.key): \(.value.url)"' "$ENDPOINTS_FILE"
}

# Add new endpoint
cmd_endpoint_add() {
    local id="$1"
    local url="$2"
    local auth="${3:-}"

    if [[ -z "$id" || -z "$url" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants endpoint add <id> <url> [auth-token]${NC}"
        exit 1
    fi

    ensure_config

    local auth_json="null"
    if [[ -n "$auth" ]]; then
        auth_json="\"$auth\""
    fi

    local tmp_file
    tmp_file=$(mktemp)
    jq --arg id "$id" --arg url "$url" --argjson auth "$auth_json" \
        '.[$id] = {"url": $url, "auth": $auth}' "$ENDPOINTS_FILE" > "$tmp_file"
    mv "$tmp_file" "$ENDPOINTS_FILE"

    echo -e "${GREEN}Added endpoint '$id' -> $url${NC}"
}

# Remove endpoint
cmd_endpoint_remove() {
    local id="$1"

    if [[ -z "$id" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants endpoint remove <id>${NC}"
        exit 1
    fi

    ensure_config

    local tmp_file
    tmp_file=$(mktemp)
    jq --arg id "$id" 'del(.[$id])' "$ENDPOINTS_FILE" > "$tmp_file"
    mv "$tmp_file" "$ENDPOINTS_FILE"

    echo -e "${GREEN}Removed endpoint '$id'${NC}"
}

# Get status from Sweatpants instance
cmd_status() {
    local endpoint_url="$1"
    local auth="$2"

    local response
    response=$(api_request GET "$endpoint_url" "/api/status" "" "$auth")

    if [[ $? -ne 0 ]]; then
        echo -e "${RED}Failed to connect to Sweatpants at $endpoint_url${NC}"
        exit 1
    fi

    echo -e "${BLUE}Sweatpants Status${NC} ($endpoint_url)"
    echo "=================="
    echo "$response" | jq .
}

# List available modules
cmd_module_list() {
    local endpoint_url="$1"
    local auth="$2"

    local response
    response=$(api_request GET "$endpoint_url" "/api/modules" "" "$auth")

    echo -e "${BLUE}Available Modules${NC}"
    echo "================="
    echo "$response" | jq -r '.modules[] | "  \(.id): \(.description // "No description")"'
}

# Run a module
cmd_run() {
    local endpoint_url="$1"
    local auth="$2"
    shift 2

    local module_id=""
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
                if [[ -z "$module_id" ]]; then
                    module_id="$1"
                fi
                shift
                ;;
        esac
    done

    if [[ -z "$module_id" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants run <module-id> [-i key=value]...${NC}"
        exit 1
    fi

    local payload
    payload=$(jq -n --arg module "$module_id" --argjson inputs "$inputs" \
        '{"module": $module, "inputs": $inputs}')

    echo -e "${BLUE}Starting job: $module_id${NC}"

    local response
    response=$(api_request POST "$endpoint_url" "/api/jobs" "$payload" "$auth")

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

# Get job logs
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
        # WebSocket streaming not implemented in basic bridge
        # Fall back to polling
        echo -e "${YELLOW}Following logs (polling mode)...${NC}"
        echo "Press Ctrl+C to stop"
        echo ""

        local last_offset=0
        while true; do
            local response
            response=$(api_request GET "$endpoint_url" "/api/jobs/$job_id/logs?offset=$last_offset" "" "$auth")

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
        response=$(api_request GET "$endpoint_url" "/api/jobs/$job_id/logs" "" "$auth")
        echo "$response" | jq -r '.logs // .'
    fi
}

# List jobs
cmd_jobs() {
    local endpoint_url="$1"
    local auth="$2"

    local response
    response=$(api_request GET "$endpoint_url" "/api/jobs" "" "$auth")

    echo -e "${BLUE}Jobs${NC}"
    echo "====="
    echo "$response" | jq -r '.jobs[] | "\(.id) [\(.status)]: \(.module) - \(.created_at // "unknown")"'
}

# Cancel a job
cmd_cancel() {
    local endpoint_url="$1"
    local auth="$2"
    local job_id="$3"

    if [[ -z "$job_id" ]]; then
        echo -e "${RED}Usage: homeboy sweatpants cancel <job-id>${NC}"
        exit 1
    fi

    local response
    response=$(api_request POST "$endpoint_url" "/api/jobs/$job_id/cancel" "" "$auth")

    echo -e "${GREEN}Cancelled job: $job_id${NC}"
}

# Show help
show_help() {
    echo "Sweatpants Bridge - Homeboy module for Sweatpants automation engine"
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
    echo "  module list                  List available modules"
    echo "  run <module> [-i k=v]...     Run a module with inputs"
    echo "  jobs                         List jobs"
    echo "  logs <job-id> [--follow]     View job logs"
    echo "  cancel <job-id>              Cancel a running job"
    echo ""
    echo "Examples:"
    echo "  homeboy sweatpants status"
    echo "  homeboy sweatpants local status"
    echo "  homeboy sweatpants run my-module -i key=value"
    echo "  homeboy sweatpants vps run scraper -i tags=lo-fi"
    echo "  homeboy sweatpants logs abc123 --follow"
}

# Determine if first argument is an endpoint or a command
is_endpoint() {
    local arg="$1"
    ensure_config
    jq -e --arg id "$arg" '.[$id]' "$ENDPOINTS_FILE" > /dev/null 2>&1
}

# Main entry point
main() {
    if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" || "$1" == "help" ]]; then
        show_help
        exit 0
    fi

    ensure_config

    # Handle endpoint management commands first
    case "$1" in
        endpoints)
            cmd_endpoints
            exit 0
            ;;
        endpoint)
            case "$2" in
                add)
                    cmd_endpoint_add "$3" "$4" "$5"
                    exit 0
                    ;;
                remove)
                    cmd_endpoint_remove "$3"
                    exit 0
                    ;;
                *)
                    echo -e "${RED}Unknown endpoint command: $2${NC}"
                    exit 1
                    ;;
            esac
            ;;
    esac

    # Determine endpoint and command
    local endpoint_id="$DEFAULT_ENDPOINT"
    local cmd=""
    local args=()

    if is_endpoint "$1"; then
        endpoint_id="$1"
        shift
    fi

    cmd="$1"
    shift
    args=("$@")

    # Get endpoint configuration
    local endpoint_url
    endpoint_url=$(get_endpoint_url "$endpoint_id")
    local auth
    auth=$(get_endpoint_auth "$endpoint_id")

    # Route to command handler
    case "$cmd" in
        status)
            cmd_status "$endpoint_url" "$auth"
            ;;
        module)
            case "${args[0]}" in
                list)
                    cmd_module_list "$endpoint_url" "$auth"
                    ;;
                *)
                    echo -e "${RED}Unknown module command: ${args[0]}${NC}"
                    exit 1
                    ;;
            esac
            ;;
        run)
            cmd_run "$endpoint_url" "$auth" "${args[@]}"
            ;;
        jobs)
            cmd_jobs "$endpoint_url" "$auth"
            ;;
        logs)
            cmd_logs "$endpoint_url" "$auth" "${args[@]}"
            ;;
        cancel)
            cmd_cancel "$endpoint_url" "$auth" "${args[@]}"
            ;;
        *)
            echo -e "${RED}Unknown command: $cmd${NC}"
            show_help
            exit 1
            ;;
    esac
}

main "$@"
