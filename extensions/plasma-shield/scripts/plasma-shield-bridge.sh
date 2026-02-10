#!/bin/bash
# Plasma Shield Bridge - Homeboy module for Plasma Shield network security
# Manages shield routers, agents, rules, and traffic logs

set -e

CONFIG_DIR="$HOME/.config/homeboy/plasma-shield"
ROUTERS_FILE="$CONFIG_DIR/routers.json"
DEFAULT_ROUTER="local"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Ensure config directory exists
ensure_config() {
    if [[ ! -d "$CONFIG_DIR" ]]; then
        mkdir -p "$CONFIG_DIR"
    fi

    if [[ ! -f "$ROUTERS_FILE" ]]; then
        echo '{
  "local": {
    "api_url": "http://127.0.0.1:9000",
    "api_key": null,
    "description": "Local development shield"
  }
}' > "$ROUTERS_FILE"
    fi
}

# Get router URL from config
get_router_url() {
    local router_id="$1"
    ensure_config

    if ! command -v jq &> /dev/null; then
        echo -e "${RED}Error: jq is required but not installed${NC}" >&2
        exit 1
    fi

    local url
    url=$(jq -r --arg id "$router_id" '.[$id].api_url // empty' "$ROUTERS_FILE")

    if [[ -z "$url" ]]; then
        echo -e "${RED}Error: Router '$router_id' not found${NC}" >&2
        echo -e "Available routers:" >&2
        jq -r 'keys[]' "$ROUTERS_FILE" >&2
        exit 1
    fi

    echo "$url"
}

# Get router API key from config
get_router_key() {
    local router_id="$1"
    jq -r --arg id "$router_id" '.[$id].api_key // empty' "$ROUTERS_FILE"
}

# Make API request to shield router
api_request() {
    local method="$1"
    local router_url="$2"
    local path="$3"
    local data="$4"
    local api_key="$5"

    local curl_args=(-s -X "$method")

    if [[ -n "$api_key" && "$api_key" != "null" ]]; then
        curl_args+=(-H "X-API-Key: $api_key")
    fi

    curl_args+=(-H "Content-Type: application/json")

    if [[ -n "$data" ]]; then
        curl_args+=(-d "$data")
    fi

    curl "${curl_args[@]}" "${router_url}${path}" 2>/dev/null
}

# List configured routers
cmd_routers() {
    ensure_config
    echo -e "${BLUE}Configured Shield Routers${NC}"
    echo "=========================="
    jq -r 'to_entries[] | "\(.key): \(.value.api_url) - \(.value.description // "No description")"' "$ROUTERS_FILE"
}

# Add new router
cmd_router_add() {
    local id="$1"
    local url="$2"
    local desc="${3:-}"
    local key="${4:-}"

    if [[ -z "$id" || -z "$url" ]]; then
        echo -e "${RED}Usage: homeboy plasma router add <id> <api-url> [description] [api-key]${NC}"
        exit 1
    fi

    ensure_config

    local key_json="null"
    if [[ -n "$key" ]]; then
        key_json="\"$key\""
    fi

    local tmp_file
    tmp_file=$(mktemp)
    jq --arg id "$id" --arg url "$url" --arg desc "$desc" --argjson key "$key_json" \
        '.[$id] = {"api_url": $url, "description": $desc, "api_key": $key}' "$ROUTERS_FILE" > "$tmp_file"
    mv "$tmp_file" "$ROUTERS_FILE"

    echo -e "${GREEN}Added router '$id' -> $url${NC}"
}

# Remove router
cmd_router_remove() {
    local id="$1"

    if [[ -z "$id" ]]; then
        echo -e "${RED}Usage: homeboy plasma router remove <id>${NC}"
        exit 1
    fi

    ensure_config

    local tmp_file
    tmp_file=$(mktemp)
    jq --arg id "$id" 'del(.[$id])' "$ROUTERS_FILE" > "$tmp_file"
    mv "$tmp_file" "$ROUTERS_FILE"

    echo -e "${GREEN}Removed router '$id'${NC}"
}

# Get shield status
cmd_status() {
    local router_url="$1"
    local api_key="$2"

    echo -e "${BLUE}Plasma Shield Status${NC}"
    echo "====================="

    # Health check
    local health
    health=$(api_request GET "$router_url" "/health" "" "$api_key")
    if [[ "$health" == "OK" ]]; then
        echo -e "Health: ${GREEN}OK${NC}"
    else
        echo -e "Health: ${RED}UNREACHABLE${NC}"
        exit 1
    fi

    # Mode
    local mode_response
    mode_response=$(api_request GET "$router_url" "/mode" "" "$api_key")
    local global_mode
    global_mode=$(echo "$mode_response" | jq -r '.global_mode // "unknown"')
    
    case "$global_mode" in
        enforce)
            echo -e "Mode: ${GREEN}ENFORCE${NC} (blocking enabled)"
            ;;
        audit)
            echo -e "Mode: ${YELLOW}AUDIT${NC} (logging only)"
            ;;
        lockdown)
            echo -e "Mode: ${RED}LOCKDOWN${NC} (all traffic blocked)"
            ;;
        *)
            echo -e "Mode: ${CYAN}$global_mode${NC}"
            ;;
    esac

    # Agent overrides
    local agent_modes
    agent_modes=$(echo "$mode_response" | jq -r '.agent_modes // {}')
    local override_count
    override_count=$(echo "$agent_modes" | jq 'length')
    
    if [[ "$override_count" -gt 0 ]]; then
        echo -e "\nAgent Overrides:"
        echo "$agent_modes" | jq -r 'to_entries[] | "  \(.key): \(.value)"'
    fi
}

# List agents (from mode data)
cmd_agents() {
    local router_url="$1"
    local api_key="$2"

    local response
    response=$(api_request GET "$router_url" "/mode" "" "$api_key")

    echo -e "${BLUE}Agents${NC}"
    echo "======="
    
    local global_mode
    global_mode=$(echo "$response" | jq -r '.global_mode // "unknown"')
    echo -e "Global mode: ${CYAN}$global_mode${NC}"
    
    echo -e "\nPer-agent modes:"
    local agent_modes
    agent_modes=$(echo "$response" | jq -r '.agent_modes // {}')
    
    if [[ $(echo "$agent_modes" | jq 'length') -eq 0 ]]; then
        echo "  (no agent-specific overrides)"
    else
        echo "$agent_modes" | jq -r 'to_entries[] | "  \(.key): \(.value)"'
    fi
}

# Get/set global mode
cmd_mode() {
    local router_url="$1"
    local api_key="$2"
    local new_mode="$3"

    if [[ -z "$new_mode" ]]; then
        # Get current mode
        local response
        response=$(api_request GET "$router_url" "/mode" "" "$api_key")
        local mode
        mode=$(echo "$response" | jq -r '.global_mode // "unknown"')
        echo "Current mode: $mode"
    else
        # Set new mode
        case "$new_mode" in
            enforce|audit|lockdown)
                local response
                response=$(api_request PUT "$router_url" "/mode" "{\"mode\": \"$new_mode\"}" "$api_key")
                echo -e "${GREEN}Mode set to: $new_mode${NC}"
                ;;
            *)
                echo -e "${RED}Invalid mode: $new_mode${NC}"
                echo "Valid modes: enforce, audit, lockdown"
                exit 1
                ;;
        esac
    fi
}

# Agent-specific operations
cmd_agent() {
    local router_url="$1"
    local api_key="$2"
    local agent_id="$3"
    local action="$4"

    if [[ -z "$agent_id" ]]; then
        echo -e "${RED}Usage: homeboy plasma agent <agent-id> <action>${NC}"
        echo "Actions: enforce, audit, lockdown, clear"
        exit 1
    fi

    case "$action" in
        enforce|audit|lockdown)
            local response
            response=$(api_request PUT "$router_url" "/agent/$agent_id/mode" "{\"mode\": \"$action\"}" "$api_key")
            echo -e "${GREEN}Agent '$agent_id' set to: $action${NC}"
            ;;
        clear)
            local response
            response=$(api_request DELETE "$router_url" "/agent/$agent_id/mode" "" "$api_key")
            echo -e "${GREEN}Agent '$agent_id' cleared (using global mode)${NC}"
            ;;
        *)
            echo -e "${RED}Unknown action: $action${NC}"
            echo "Valid actions: enforce, audit, lockdown, clear"
            exit 1
            ;;
    esac
}

# View traffic logs
cmd_logs() {
    local router_url="$1"
    local api_key="$2"
    shift 2

    local limit=50
    local follow=false
    local agent=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --limit|-l)
                limit="$2"
                shift 2
                ;;
            --follow|-f)
                follow=true
                shift
                ;;
            --agent|-a)
                agent="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    local path="/logs?limit=$limit"
    if [[ -n "$agent" ]]; then
        path="${path}&agent=$agent"
    fi

    if [[ "$follow" == true ]]; then
        echo -e "${YELLOW}Following logs (polling mode)...${NC}"
        echo "Press Ctrl+C to stop"
        echo ""

        while true; do
            local response
            response=$(api_request GET "$router_url" "$path" "" "$api_key")
            echo "$response" | jq -r '.[] | "\(.timestamp) [\(.action)] \(.domain) - \(.reason // "")"' 2>/dev/null || echo "$response"
            sleep 2
        done
    else
        local response
        response=$(api_request GET "$router_url" "$path" "" "$api_key")
        echo -e "${BLUE}Traffic Logs${NC} (last $limit)"
        echo "============="
        echo "$response" | jq -r '.[] | "\(.timestamp) [\(.action)] \(.domain) - \(.reason // "")"' 2>/dev/null || echo "$response"
    fi
}

# List rules
cmd_rules() {
    local router_url="$1"
    local api_key="$2"
    local action="$3"
    shift 3

    case "$action" in
        list|"")
            local response
            response=$(api_request GET "$router_url" "/rules" "" "$api_key")
            echo -e "${BLUE}Blocking Rules${NC}"
            echo "==============="
            echo "$response" | jq -r '.rules[] | "[\(.id)] \(.domain // .pattern) -> \(.action) (\(.description // "no description"))"' 2>/dev/null || echo "$response"
            ;;
        add)
            # homeboy plasma rules add --domain evil.com --action block --desc "Block evil"
            local domain="" pattern="" rule_action="block" desc=""
            while [[ $# -gt 0 ]]; do
                case "$1" in
                    --domain|-d) domain="$2"; shift 2 ;;
                    --pattern|-p) pattern="$2"; shift 2 ;;
                    --action|-a) rule_action="$2"; shift 2 ;;
                    --desc) desc="$2"; shift 2 ;;
                    *) shift ;;
                esac
            done
            
            local payload="{\"action\": \"$rule_action\", \"description\": \"$desc\""
            if [[ -n "$domain" ]]; then
                payload="$payload, \"domain\": \"$domain\"}"
            elif [[ -n "$pattern" ]]; then
                payload="$payload, \"pattern\": \"$pattern\"}"
            else
                echo -e "${RED}Must specify --domain or --pattern${NC}"
                exit 1
            fi
            
            local response
            response=$(api_request POST "$router_url" "/rules" "$payload" "$api_key")
            echo -e "${GREEN}Rule added${NC}"
            echo "$response" | jq .
            ;;
        remove)
            local rule_id="$1"
            if [[ -z "$rule_id" ]]; then
                echo -e "${RED}Usage: homeboy plasma rules remove <rule-id>${NC}"
                exit 1
            fi
            local response
            response=$(api_request DELETE "$router_url" "/rules/$rule_id" "" "$api_key")
            echo -e "${GREEN}Rule '$rule_id' removed${NC}"
            ;;
        *)
            echo -e "${RED}Unknown rules action: $action${NC}"
            echo "Valid actions: list, add, remove"
            exit 1
            ;;
    esac
}

# Show help
show_help() {
    echo "Plasma Shield - Network security control for AI agent fleets"
    echo ""
    echo "Usage: homeboy plasma [router] <command> [args...]"
    echo ""
    echo "Router Management:"
    echo "  routers                      List configured routers"
    echo "  router add <id> <url>        Add new router"
    echo "  router remove <id>           Remove router"
    echo ""
    echo "Commands (use with optional router prefix):"
    echo "  status                       Show shield status"
    echo "  agents                       List agents and their modes"
    echo "  mode [enforce|audit|lockdown]  Get or set global mode"
    echo "  agent <id> <action>          Agent-specific mode (enforce|audit|lockdown|clear)"
    echo "  logs [--limit N] [--follow]  View traffic logs"
    echo "  rules [list|add|remove]      Manage blocking rules"
    echo ""
    echo "Examples:"
    echo "  homeboy plasma status"
    echo "  homeboy plasma mode enforce"
    echo "  homeboy plasma agent sarai lockdown"
    echo "  homeboy plasma logs --limit 100 --follow"
    echo "  homeboy plasma rules add --domain evil.com --action block"
    echo "  homeboy plasma prod status"
}

# Determine if first argument is a router or a command
is_router() {
    local arg="$1"
    ensure_config
    jq -e --arg id "$arg" '.[$id]' "$ROUTERS_FILE" > /dev/null 2>&1
}

# Main entry point
main() {
    if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" || "$1" == "help" ]]; then
        show_help
        exit 0
    fi

    ensure_config

    # Handle router management commands first
    case "$1" in
        routers)
            cmd_routers
            exit 0
            ;;
        router)
            case "$2" in
                add)
                    cmd_router_add "$3" "$4" "$5" "$6"
                    exit 0
                    ;;
                remove)
                    cmd_router_remove "$3"
                    exit 0
                    ;;
                *)
                    echo -e "${RED}Unknown router command: $2${NC}"
                    exit 1
                    ;;
            esac
            ;;
    esac

    # Determine router and command
    local router_id="$DEFAULT_ROUTER"
    local cmd=""
    local args=()

    if is_router "$1"; then
        router_id="$1"
        shift
    fi

    cmd="$1"
    shift
    args=("$@")

    # Get router configuration
    local router_url
    router_url=$(get_router_url "$router_id")
    local api_key
    api_key=$(get_router_key "$router_id")

    # Route to command handler
    case "$cmd" in
        status)
            cmd_status "$router_url" "$api_key"
            ;;
        agents)
            cmd_agents "$router_url" "$api_key"
            ;;
        mode)
            cmd_mode "$router_url" "$api_key" "${args[0]}"
            ;;
        agent)
            cmd_agent "$router_url" "$api_key" "${args[@]}"
            ;;
        logs)
            cmd_logs "$router_url" "$api_key" "${args[@]}"
            ;;
        rules)
            cmd_rules "$router_url" "$api_key" "${args[@]}"
            ;;
        *)
            echo -e "${RED}Unknown command: $cmd${NC}"
            show_help
            exit 1
            ;;
    esac
}

main "$@"
