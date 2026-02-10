#!/bin/bash
# Plasma Shield - Desktop module runner
# Outputs JSON for Homeboy Desktop table display

set -e

# Settings from environment (injected by Homeboy)
ROUTER_URL="${HOMEBOY_SETTING_ROUTER_URL:-http://localhost:9000}"
API_KEY="${HOMEBOY_SETTING_API_KEY:-}"

# Inputs from environment
VIEW="${HOMEBOY_INPUT_VIEW:-status}"
AGENT="${HOMEBOY_INPUT_AGENT:-}"
LIMIT="${HOMEBOY_INPUT_LIMIT:-50}"

# Build curl headers
CURL_ARGS=(-s)
if [[ -n "$API_KEY" ]]; then
    CURL_ARGS+=(-H "X-API-Key: $API_KEY")
fi

# API request helper
api_get() {
    local path="$1"
    curl "${CURL_ARGS[@]}" "${ROUTER_URL}${path}" 2>/dev/null
}

# Output success JSON
output_success() {
    local results="$1"
    echo "{\"success\": true, \"results\": $results}"
}

# Output error JSON  
output_error() {
    local message="$1"
    echo "{\"success\": false, \"errors\": [\"$message\"]}"
}

# Get shield status
get_status() {
    local health
    health=$(api_get "/health")
    
    if [[ "$health" != "OK" ]]; then
        output_error "Shield router unreachable at $ROUTER_URL"
        exit 0
    fi
    
    local mode_data
    mode_data=$(api_get "/mode")
    
    local global_mode
    global_mode=$(echo "$mode_data" | jq -r '.global_mode // "unknown"')
    
    # Build results array
    local results="["
    results+="{\"agent\": \"(global)\", \"mode\": \"$global_mode\", \"domain\": \"-\", \"action\": \"-\", \"timestamp\": \"now\", \"reason\": \"Global shield mode\"}"
    
    # Add per-agent overrides
    local agent_modes
    agent_modes=$(echo "$mode_data" | jq -r '.agent_modes // {}')
    
    local first=false
    while IFS='=' read -r agent mode; do
        if [[ -n "$agent" ]]; then
            results+=",{\"agent\": \"$agent\", \"mode\": \"$mode\", \"domain\": \"-\", \"action\": \"override\", \"timestamp\": \"now\", \"reason\": \"Agent-specific mode\"}"
        fi
    done < <(echo "$agent_modes" | jq -r 'to_entries[] | "\(.key)=\(.value)"')
    
    results+="]"
    output_success "$results"
}

# Get agent modes
get_agents() {
    local mode_data
    mode_data=$(api_get "/mode")
    
    if [[ -z "$mode_data" ]]; then
        output_error "Failed to fetch agent modes"
        exit 0
    fi
    
    local global_mode
    global_mode=$(echo "$mode_data" | jq -r '.global_mode // "unknown"')
    
    local results="["
    results+="{\"agent\": \"(global default)\", \"mode\": \"$global_mode\", \"domain\": \"-\", \"action\": \"-\", \"timestamp\": \"-\", \"reason\": \"Applied to all agents without override\"}"
    
    local agent_modes
    agent_modes=$(echo "$mode_data" | jq -r '.agent_modes // {}')
    
    while IFS='=' read -r agent mode; do
        if [[ -n "$agent" ]]; then
            results+=",{\"agent\": \"$agent\", \"mode\": \"$mode\", \"domain\": \"-\", \"action\": \"override\", \"timestamp\": \"-\", \"reason\": \"Agent-specific override\"}"
        fi
    done < <(echo "$agent_modes" | jq -r 'to_entries[] | "\(.key)=\(.value)"')
    
    results+="]"
    output_success "$results"
}

# Get traffic logs
get_logs() {
    local path="/logs?limit=$LIMIT"
    if [[ -n "$AGENT" ]]; then
        path="${path}&agent=$AGENT"
    fi
    
    local logs
    logs=$(api_get "$path")
    
    if [[ -z "$logs" || "$logs" == "null" ]]; then
        output_success "[]"
        exit 0
    fi
    
    # Transform logs to match our schema
    local results
    results=$(echo "$logs" | jq '[.[] | {
        timestamp: .timestamp,
        agent: (.agent_token // "-"),
        domain: .domain,
        action: .action,
        mode: "-",
        reason: (.reason // "-")
    }]' 2>/dev/null || echo "[]")
    
    output_success "$results"
}

# Get blocking rules
get_rules() {
    local rules
    rules=$(api_get "/rules")
    
    if [[ -z "$rules" || "$rules" == "null" ]]; then
        output_success "[]"
        exit 0
    fi
    
    # Transform rules to match our schema
    local results
    results=$(echo "$rules" | jq '[(.rules // [])[] | {
        timestamp: "-",
        agent: "-",
        domain: (.domain // .pattern // "-"),
        action: .action,
        mode: (if .enabled then "enabled" else "disabled" end),
        reason: (.description // "-")
    }]' 2>/dev/null || echo "[]")
    
    output_success "$results"
}

# Main
case "$VIEW" in
    status)
        get_status
        ;;
    agents)
        get_agents
        ;;
    logs)
        get_logs
        ;;
    rules)
        get_rules
        ;;
    *)
        output_error "Unknown view: $VIEW"
        ;;
esac
