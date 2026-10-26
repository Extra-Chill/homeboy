#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/lib/repository-file-discovery.sh"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
REPO="$TMP_DIR/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" config user.name 'Smoke Test'
git -C "$REPO" config user.email 'smoke@example.test'
printf '<?php\n' > "$REPO/keep.php"
printf '<?php\n' > "$REPO/delete.php"
git -C "$REPO" add keep.php delete.php
git -C "$REPO" commit -qm 'Add PHP files'
rm "$REPO/delete.php"

mapfile -d '' -t found < <(homeboy_discover_repository_files "$REPO" '*.php')
if [ "${#found[@]}" -ne 1 ] || [ "${found[0]}" != keep.php ]; then
    printf 'Unexpected discovery result: %s\n' "${found[*]}" >&2
    exit 1
fi

echo "repository file discovery smoke passed"
