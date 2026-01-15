#!/usr/bin/env bash
set -euo pipefail

if [[ -z "${HOMEBOY_SETTINGS_JSON:-}" ]]; then
  echo "Missing HOMEBOY_SETTINGS_JSON" >&2
  exit 1
fi

payload=$(echo "$HOMEBOY_SETTINGS_JSON" | jq -r '.release // empty')
if [[ -z "$payload" ]]; then
  echo "Missing release payload" >&2
  exit 1
fi

tag=$(echo "$payload" | jq -r '.tag // empty')
notes=$(echo "$payload" | jq -r '.notes // empty')

if [[ -z "$tag" ]]; then
  echo "Release tag missing" >&2
  exit 1
fi

artifact_types=$(echo "$HOMEBOY_SETTINGS_JSON" | jq -c '.config.artifactTypes // empty')
artifacts=$(echo "$payload" | jq -r '.artifacts // [] | .[] | .path')

if [[ -n "$artifact_types" && "$artifact_types" != "null" ]]; then
  artifacts=$(echo "$payload" | jq -r --argjson allowed "$artifact_types" '.artifacts[] | select(.type as $t | $allowed | index($t)) | .path')
fi

if [[ -z "$artifacts" ]]; then
  echo "No artifacts to upload" >&2
  exit 1
fi

notes_file=$(mktemp)
trap 'rm -f "$notes_file"' EXIT

echo "$notes" > "$notes_file"

gh release create "$tag" --title "$tag" --notes-file "$notes_file" $artifacts
