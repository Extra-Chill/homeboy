#!/usr/bin/env bash

# Shared component detection for WordPress extension scripts.
#
# Detects whether a component is a plugin or theme and extracts
# metadata from the header (Plugin Name, Text Domain, Version, etc.)
#
# Usage: source this file, then call:
#   homeboy_detect_component <path>
#
# After calling, these variables are set:
#   HOMEBOY_COMPONENT_TYPE             — "plugin" or "theme" or ""
#   HOMEBOY_COMPONENT_MAIN_FILE        — path to the main plugin/theme file
#   HOMEBOY_COMPONENT_NAME             — Plugin Name / Theme Name value
#   HOMEBOY_COMPONENT_TEXT_DOMAIN      — Text Domain value (may be empty)
#   HOMEBOY_COMPONENT_VERSION          — Version value (may be empty)
#   HOMEBOY_COMPONENT_REQUIRES_PLUGINS — comma-separated plugin slugs (may be empty)

homeboy_detect_component() {
    local component_path="${1:-.}"

    HOMEBOY_COMPONENT_TYPE=""
    HOMEBOY_COMPONENT_MAIN_FILE=""
    HOMEBOY_COMPONENT_NAME=""
    HOMEBOY_COMPONENT_TEXT_DOMAIN=""
    HOMEBOY_COMPONENT_VERSION=""
    HOMEBOY_COMPONENT_REQUIRES_PLUGINS=""

    # Check for theme first (style.css with "Theme Name:")
    if [ -f "${component_path}/style.css" ]; then
        if grep -q "Theme Name:" "${component_path}/style.css" 2>/dev/null; then
            HOMEBOY_COMPONENT_TYPE="theme"
            HOMEBOY_COMPONENT_MAIN_FILE="${component_path}/style.css"
            HOMEBOY_COMPONENT_NAME=$(grep -m1 "Theme Name:" "${component_path}/style.css" | sed 's/.*Theme Name:[[:space:]]*//' | sed 's/[[:space:]]*$//' | tr -d '\r')
            HOMEBOY_COMPONENT_TEXT_DOMAIN=$(grep -m1 "Text Domain:" "${component_path}/style.css" | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
            HOMEBOY_COMPONENT_VERSION=$(grep -m1 "Version:" "${component_path}/style.css" | sed 's/.*Version:[[:space:]]*//' | tr -d ' \r')
            HOMEBOY_COMPONENT_REQUIRES_PLUGINS=$(grep -m1 "Requires Plugins:" "${component_path}/style.css" 2>/dev/null | sed 's/.*Requires Plugins:[[:space:]]*//' | sed 's/[[:space:]]*$//' | tr -d '\r')
            return 0
        fi
    fi

    # Check for plugin (*.php with "Plugin Name:" in root)
    local main_file
    main_file=$(find "$component_path" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)

    if [ -n "$main_file" ]; then
        HOMEBOY_COMPONENT_TYPE="plugin"
        HOMEBOY_COMPONENT_MAIN_FILE="$main_file"
        HOMEBOY_COMPONENT_NAME=$(grep -m1 "Plugin Name:" "$main_file" | sed 's/.*Plugin Name:[[:space:]]*//' | sed 's/[[:space:]]*$//' | tr -d '\r')
        HOMEBOY_COMPONENT_TEXT_DOMAIN=$(grep -m1 "Text Domain:" "$main_file" | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
        HOMEBOY_COMPONENT_VERSION=$(grep -m1 "Version:" "$main_file" | sed 's/.*Version:[[:space:]]*//' | tr -d ' \r')
        HOMEBOY_COMPONENT_REQUIRES_PLUGINS=$(grep -m1 "Requires Plugins:" "$main_file" 2>/dev/null | sed 's/.*Requires Plugins:[[:space:]]*//' | sed 's/[[:space:]]*$//' | tr -d '\r')
        return 0
    fi

    # Not a recognized WordPress component
    return 1
}
