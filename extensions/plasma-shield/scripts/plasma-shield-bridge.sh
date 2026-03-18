#!/bin/bash
# Plasma Shield Bridge - Homeboy extension for Plasma Shield network security
# Manages shield routers, agents, rules, and traffic logs

set -e

# Configure bridge framework
BRIDGE_NAME="plasma-shield"
BRIDGE_ENTITY="router"
BRIDGE_URL_FIELD="api_url"
BRIDGE_AUTH_FIELD="api_key"
BRIDGE_AUTH_HEADER="X-API-Key:"
BRIDGE_DEFAULT_ID="local"
BRIDGE_DEFAULT_CONFIG='{
  "local": {
    "api_url": "http://127.0.0.1:9000",
    "api_key": null,
    "description": "Local development shield"
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

# --- Plasma Shield-specific commands ---

cmd_status() {
    local router_url="$1"
    local api_key="$2"

    echo -e "${BLUE}Plasma Shield Status${NC}"
    echo "====================="

    local health
    health=$(bridge_api_request GET "$router_url" "/health" "" "$api_key")
    if [[ "$health" == "OK" ]]; then
        echo -e "Health: ${GREEN}OK${NC}"
    else
        echo -e "Health: ${RED}UNREACHABLE${NC}"
        exit 1
    fi

    local mode_response
    mode_response=$(bridge_api_request GET "$router_url" "/mode" "" "$api_key")
    local global_mode
    global_mode=$(echo "$mode_response" | jq -r '.global_mode // "unknown"')

    case "$global_mode" in
        enforce)  echo -e "Mode: ${GREEN}ENFORCE${NC} (blocking enabled)" ;;
        audit)    echo -e "Mode: ${YELLOW}AUDIT${NC} (logging only)" ;;
        lockdown) echo -e "Mode: ${RED}LOCKDOWN${NC} (all traffic blocked)" ;;
        *)        echo -e "Mode: ${CYAN}$global_mode${NC}" ;;
    esac

    local agent_modes
    agent_modes=$(echo "$mode_response" | jq -r '.agent_modes // {}')
    local override_count
    override_count=$(echo "$agent_modes" | jq 'length')

    if [[ "$override_count" -gt 0 ]]; then
        echo -e "\nAgent Overrides:"
        echo "$agent_modes" | jq -r 'to_entries[] | "  \(.key): \(.value)"'
    fi
}

cmd_agents() {
    local router_url="$1"
    local api_key="$2"

    local response
    response=$(bridge_api_request GET "$router_url" "/mode" "" "$api_key")

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

cmd_mode() {
    local router_url="$1"
    local api_key="$2"
    local new_mode="$3"

    if [[ -z "$new_mode" ]]; then
        local response
        response=$(bridge_api_request GET "$router_url" "/mode" "" "$api_key")
        local mode
        mode=$(echo "$response" | jq -r '.global_mode // "unknown"')
        echo "Current mode: $mode"
    else
        case "$new_mode" in
            enforce|audit|lockdown)
                bridge_api_request PUT "$router_url" "/mode" "{\"mode\": \"$new_mode\"}" "$api_key" > /dev/null
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
            bridge_api_request PUT "$router_url" "/agent/$agent_id/mode" "{\"mode\": \"$action\"}" "$api_key" > /dev/null
            echo -e "${GREEN}Agent '$agent_id' set to: $action${NC}"
            ;;
        clear)
            bridge_api_request DELETE "$router_url" "/agent/$agent_id/mode" "" "$api_key" > /dev/null
            echo -e "${GREEN}Agent '$agent_id' cleared (using global mode)${NC}"
            ;;
        *)
            echo -e "${RED}Unknown action: $action${NC}"
            echo "Valid actions: enforce, audit, lockdown, clear"
            exit 1
            ;;
    esac
}

cmd_logs() {
    local router_url="$1"
    local api_key="$2"
    shift 2

    local limit=50
    local follow=false
    local agent=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --limit|-l) limit="$2"; shift 2 ;;
            --follow|-f) follow=true; shift ;;
            --agent|-a) agent="$2"; shift 2 ;;
            *) shift ;;
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
            response=$(bridge_api_request GET "$router_url" "$path" "" "$api_key")
            echo "$response" | jq -r '.[] | "\(.timestamp) [\(.action)] \(.domain) - \(.reason // "")"' 2>/dev/null || echo "$response"
            sleep 2
        done
    else
        local response
        response=$(bridge_api_request GET "$router_url" "$path" "" "$api_key")
        echo -e "${BLUE}Traffic Logs${NC} (last $limit)"
        echo "============="
        echo "$response" | jq -r '.[] | "\(.timestamp) [\(.action)] \(.domain) - \(.reason // "")"' 2>/dev/null || echo "$response"
    fi
}

cmd_rules() {
    local router_url="$1"
    local api_key="$2"
    local action="$3"
    shift 3

    case "$action" in
        list|"")
            local response
            response=$(bridge_api_request GET "$router_url" "/rules" "" "$api_key")
            echo -e "${BLUE}Blocking Rules${NC}"
            echo "==============="
            echo "$response" | jq -r '.rules[] | "[\(.id)] \(.domain // .pattern) -> \(.action) (\(.description // "no description"))"' 2>/dev/null || echo "$response"
            ;;
        add)
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
            response=$(bridge_api_request POST "$router_url" "/rules" "$payload" "$api_key")
            echo -e "${GREEN}Rule added${NC}"
            echo "$response" | jq .
            ;;
        remove)
            local rule_id="$1"
            if [[ -z "$rule_id" ]]; then
                echo -e "${RED}Usage: homeboy plasma rules remove <rule-id>${NC}"
                exit 1
            fi
            bridge_api_request DELETE "$router_url" "/rules/$rule_id" "" "$api_key" > /dev/null
            echo -e "${GREEN}Rule '$rule_id' removed${NC}"
            ;;
        *)
            echo -e "${RED}Unknown rules action: $action${NC}"
            echo "Valid actions: list, add, remove"
            exit 1
            ;;
    esac
}

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

# --- Main entry point ---

main() {
    if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" || "$1" == "help" ]]; then
        show_help
        exit 0
    fi

    bridge_ensure_config

    # Handle entity management commands
    case "$1" in
        routers)
            bridge_cmd_list
            exit 0
            ;;
        router)
            case "$2" in
                add)    bridge_cmd_add "$3" "$4" "$5"; exit 0 ;;
                remove) bridge_cmd_remove "$3"; exit 0 ;;
                *)      echo -e "${RED}Unknown router command: $2${NC}"; exit 1 ;;
            esac
            ;;
    esac

    # Resolve router from args
    local args=("$@")
    bridge_resolve_entity args
    set -- "${args[@]}"

    local cmd="$1"
    shift

    # Route to command handler
    case "$cmd" in
        status)  cmd_status "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" ;;
        agents)  cmd_agents "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" ;;
        mode)    cmd_mode "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "${1:-}" ;;
        agent)   cmd_agent "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "$@" ;;
        logs)    cmd_logs "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "$@" ;;
        rules)   cmd_rules "$BRIDGE_RESOLVED_URL" "$BRIDGE_RESOLVED_AUTH" "$@" ;;
        *)       echo -e "${RED}Unknown command: $cmd${NC}"; show_help; exit 1 ;;
    esac
}

main "$@"
