#!/usr/bin/env bash

set -euo pipefail

REPO="Extra-Chill/homeboy"

if [ "${HOMEBOY_VERSION}" = "latest" ]; then
  TAG=$(gh api "repos/${REPO}/releases/latest" --jq '.tag_name' 2>/dev/null || true)
  if [ -z "${TAG}" ]; then
    echo "::error::Could not determine latest Homeboy release"
    exit 1
  fi
else
  TAG="v${HOMEBOY_VERSION#v}"
fi

echo "resolved-version=${TAG#v}" >> "${GITHUB_OUTPUT}"
