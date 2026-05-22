#!/usr/bin/env bash
set -euo pipefail

EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_HOME="$(mktemp -d)"
trap 'rm -rf "$TMP_HOME"' EXIT

python3 - "$EXTENSION_DIR/wordpress.json" <<'PY'
import fnmatch
import json
import sys
from pathlib import Path

manifest = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
patterns = manifest["audit"].get("ignore_claim_patterns", [])

def matches_any(path):
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)

for placeholder in [
    "daily/YYYY/MM/DD.md",
    "wp-content/plugins/<slug>/README.md",
    "records/{id}/index.md",
]:
    if not matches_any(placeholder):
        raise SystemExit(f"placeholder doc reference is not ignored: {placeholder}")

if matches_any("docs/missing-real-file.md"):
    raise SystemExit("real broken doc reference is hidden by placeholder ignore patterns")
PY

COMPONENT_DIR="$TMP_HOME/component"
EXTENSIONS_DIR="$TMP_HOME/.config/homeboy/extensions"
mkdir -p "$COMPONENT_DIR/docs" "$EXTENSIONS_DIR"
ln -s "$EXTENSION_DIR" "$EXTENSIONS_DIR/wordpress"

cat > "$COMPONENT_DIR/homeboy.json" <<'JSON'
{"id":"doc-placeholder-smoke","extensions":{"wordpress":{}}}
JSON

cat > "$COMPONENT_DIR/docs/guide.md" <<'MD'
# Doc Reference Real Broken References

- A real missing reference still fails: `docs/missing-real-file.md`.
MD

OUTPUT="$TMP_HOME/audit.json"
set +e
HOME="$TMP_HOME" homeboy audit \
  --path "$COMPONENT_DIR" \
  --extension wordpress \
  --only broken_doc_reference \
  --ignore-baseline \
  --json-summary \
  --force-hot \
  --output "$OUTPUT" >/dev/null
STATUS=$?
set -e

if [ "$STATUS" -eq 0 ]; then
  echo "Expected one real broken doc reference finding, got success" >&2
  exit 1
fi

python3 - "$OUTPUT" <<'PY'
import json
import sys
from pathlib import Path

data = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
findings = data.get("data", data).get("top_findings", [])
descriptions = "\n".join(finding.get("description", "") for finding in findings)

if "docs/missing-real-file.md" not in descriptions:
    raise SystemExit("real broken file reference was not reported")

if len(findings) != 1:
    raise SystemExit(f"expected exactly one broken reference, got {len(findings)}")

print("wordpress doc real broken reference smoke passed")
PY
