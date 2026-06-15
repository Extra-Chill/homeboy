#!/usr/bin/env bash
set -euo pipefail

if ! command -v gh >/dev/null 2>&1; then
  jq -cn '{success:false,status:"missing_tool",reason:"gh CLI is required to publish WordPress release assets and mirror branches."}'
  exit 0
fi

if [[ -n "${GH_TOKEN:-}" ]]; then
  jq -cn '{success:true,token_source:"GH_TOKEN"}'
  exit 0
fi

if gh auth token >/dev/null 2>&1; then
  jq -cn '{success:true,token_source:"gh auth token"}'
  exit 0
fi

jq -cn '{success:false,status:"missing_secret",reason:"GH_TOKEN is required to push the WordPress release-latest mirror. Set GH_TOKEN with `export GH_TOKEN=\"$(gh auth token)\"`, or run `gh auth login` and rerun the release."}'
