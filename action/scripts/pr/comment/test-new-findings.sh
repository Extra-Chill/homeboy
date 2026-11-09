#!/usr/bin/env bash
set -euo pipefail
ROOT="${GITHUB_ACTION_PATH:?}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export OUTPUT_DIR="$TMP/current" RESULTS='{"review lint":"fail"}' COMMANDS='review lint'
mkdir -p "$OUTPUT_DIR" "$TMP/base"
python3 - "$TMP" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
old = {'file': 'old.php', 'line': 1, 'rule': 'old', 'message': 'old debt', 'fingerprint': 'old'}
new = [dict(file='new.php', line=i+1, rule='new', message='new item', fingerprint=str(i), severity='warning') for i in range(60)]
new[-1]['severity'] = 'error'
(root/'base/review-lint.json').write_text(json.dumps({'success': False, 'data': {'lint_findings': [old]}}))
(root/'current/review-lint.json').write_text(json.dumps({'success': False, 'data': {'lint_findings': [old] + new}}))
PY
python3 "$ROOT/scripts/core/apply-differential-gate.py" "$RESULTS" "$OUTPUT_DIR" "$TMP/base" >/dev/null
python3 "$ROOT/scripts/pr/comment/new-findings.py" > "$TMP/annotations"
[ "$(wc -l < "$TMP/annotations")" -eq 50 ]
head -1 "$TMP/annotations" | grep -F '::error ' >/dev/null
[ "$(grep -c '^- `' "$OUTPUT_DIR/new-failures.md")" -eq 10 ]
! grep -F 'old debt' "$OUTPUT_DIR/new-failures.md"
python3 - "$OUTPUT_DIR/differential-comment.json" <<'PY'
import json, sys
v = json.load(open(sys.argv[1]))['commands']['review lint']
assert v['new_findings'] == 60
assert len(v['new_finding_details']) == 60
PY
printf 'PASS: fingerprint subtraction, comment cap, annotation cap and severity order\n'
