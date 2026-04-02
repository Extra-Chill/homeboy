#!/usr/bin/env bash
set -euo pipefail

python3 - <<'PY'
import json
import re
import sys

content = sys.stdin.read()

functions = re.findall(r'^func\s+(\w+)\s*\(', content, re.MULTILINE)
structs = re.findall(r'^type\s+(\w+)\s+struct\s*\{', content, re.MULTILINE)
interfaces = re.findall(r'^type\s+(\w+)\s+interface\s*\{', content, re.MULTILINE)

print(json.dumps({
    "symbols": {
        "functions": functions,
        "structs": structs,
        "interfaces": interfaces,
    }
}))
PY
