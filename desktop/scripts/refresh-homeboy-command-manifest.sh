#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="$ROOT/tests/fixtures/homeboy-command-manifest.txt"

usage() {
  echo "Usage: $0 [--check]" >&2
  exit 2
}

check=0
if [[ $# -gt 1 ]]; then
  usage
fi
if [[ $# -eq 1 ]]; then
  if [[ "$1" != "--check" ]]; then
    usage
  fi
  check=1
fi

if ! command -v homeboy >/dev/null 2>&1; then
  echo "homeboy is not installed" >&2
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

homeboy contract manifest | python3 -c '
import json
import sys

payload = json.load(sys.stdin)
commands = payload["data"]["commands"]

def parts(path):
    if isinstance(path, str):
        return [part for part in path.split() if part]
    if isinstance(path, list):
        return [str(part) for part in path if str(part)]
    return []

# Extension-provided commands depend on which extensions are installed, so
# the fixture covers only core commands to stay identical in every environment.
def walk(node):
    if node.get("extension"):
        return
    path = parts(node.get("path"))
    if path:
        yield " ".join(path)
    for child in node.get("subcommands") or []:
        yield from walk(child)

seen = sorted({path for command in commands for path in walk(command)})
sys.stdout.write("\n".join(seen))
if seen:
    sys.stdout.write("\n")
' > "$tmp"

if [[ "$check" -eq 1 ]]; then
  if [[ ! -f "$FIXTURE" ]] || ! cmp -s "$tmp" "$FIXTURE"; then
    echo "homeboy command manifest fixture is stale: $FIXTURE" >&2
    diff -u "$FIXTURE" "$tmp" >&2 || true
    exit 1
  fi
  exit 0
fi

mkdir -p "$(dirname "$FIXTURE")"
cp "$tmp" "$FIXTURE"
