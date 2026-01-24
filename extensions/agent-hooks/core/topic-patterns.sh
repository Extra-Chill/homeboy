#!/usr/bin/env bash
# Topic detection patterns for user prompt analysis
# Sourced by user-prompt-submit.sh to provide workflow reminders
#
# SYNC NOTE: TypeScript equivalent at ../opencode/homeboy-plugin.ts
# When modifying patterns, update both files.

# Check if prompt mentions version/release workflow topics
# Arguments: $1 = user prompt text (lowercase)
# Returns: 0 if detected, 1 otherwise
check_version_release_topic() {
    local prompt="$1"

    # Keywords: version, bump, release, changelog, tag
    if [[ "$prompt" =~ (version|bump|release|changelog|tag) ]]; then
        return 0
    fi
    return 1
}

# Check if prompt mentions deploy workflow topics
# Arguments: $1 = user prompt text (lowercase)
# Returns: 0 if detected, 1 otherwise
check_deploy_topic() {
    local prompt="$1"

    # Keywords: deploy, deployment, push to server, push to production
    if [[ "$prompt" =~ (deploy|deployment|push[[:space:]]to[[:space:]](server|production)) ]]; then
        return 0
    fi
    return 1
}

# Check if prompt mentions build workflow topics
# Arguments: $1 = user prompt text (lowercase)
# Returns: 0 if detected, 1 otherwise
check_build_topic() {
    local prompt="$1"

    # Keywords: build, compile, package, artifact
    if [[ "$prompt" =~ (build|compile|package|artifact) ]]; then
        return 0
    fi
    return 1
}

# Get workflow reminder for version/release topic
get_version_release_reminder() {
    cat <<'EOF'
Version & Release Workflow

  homeboy changelog      Update changelog with recent changes
  homeboy version bump   Increment version (patch|minor|major)
  homeboy release        Full release pipeline

Order: changelog -> version bump -> release
EOF
}

# Get workflow reminder for deploy topic
get_deploy_reminder() {
    cat <<'EOF'
Deploy Workflow

  homeboy deploy <component>   Deploy to configured server

Run 'homeboy init' to see available components and servers.
EOF
}

# Get workflow reminder for build topic
get_build_reminder() {
    cat <<'EOF'
Build Workflow

  homeboy build <component>   Create production build artifact

Output: /build/<component>.zip
EOF
}

# Check all topics and return combined reminders
# Arguments: $1 = user prompt text
# Returns: reminder messages (may be empty)
check_all_topics() {
    local prompt="$1"
    local prompt_lower
    prompt_lower=$(echo "$prompt" | tr '[:upper:]' '[:lower:]')

    local reminders=""

    if check_version_release_topic "$prompt_lower"; then
        reminders="$(get_version_release_reminder)"
    fi

    if check_deploy_topic "$prompt_lower"; then
        if [[ -n "$reminders" ]]; then
            reminders="$reminders"$'\n\n'"$(get_deploy_reminder)"
        else
            reminders="$(get_deploy_reminder)"
        fi
    fi

    if check_build_topic "$prompt_lower"; then
        if [[ -n "$reminders" ]]; then
            reminders="$reminders"$'\n\n'"$(get_build_reminder)"
        else
            reminders="$(get_build_reminder)"
        fi
    fi

    echo "$reminders"
}
