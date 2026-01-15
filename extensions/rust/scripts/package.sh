#!/usr/bin/env bash
set -euo pipefail

dist build --output-format=json > dist-manifest.json

jq -c '[.upload_files[] | {path: ., type: (if endswith(".rb") then "homebrew" else "binary" end), platform: null}]' dist-manifest.json
