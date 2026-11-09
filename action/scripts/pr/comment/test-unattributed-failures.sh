#!/usr/bin/env bash
set -euo pipefail
ROOT="${GITHUB_ACTION_PATH:-$(cd "$(dirname "$0")/../../.." && pwd)}"
export ROOT
python3 - <<'PY'
import json
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(os.environ['ROOT'])
with tempfile.TemporaryDirectory() as tmp:
    current, base = Path(tmp) / 'current', Path(tmp) / 'base'
    current.mkdir()
    base.mkdir()
    for command in ('review lint', 'review test', 'build'):
        stem = command.replace(' ', '-')
        (current / f'{stem}.log').write_text('\n'.join(f'old {i}' for i in range(12)) + '\ncompile error @someone ```\n')
        for baseline_status in ('pass', 'fail', 'missing'):
            metadata = {command: {'status': baseline_status, 'exit_code': 0 if baseline_status == 'pass' else 1}}
            (base / 'baseline-status.json').write_text(json.dumps(metadata if baseline_status != 'missing' else {}))
            subprocess.run(['python3', str(root / 'scripts/core/apply-differential-gate.py'),
                            json.dumps({command: 'fail'}), str(current), str(base)], check=True, capture_output=True)
            verdict = json.loads((current / 'differential-comment.json').read_text())['commands'][command]
            env = dict(os.environ, OUTPUT_DIR=str(current), COMMANDS=command, RESULTS=json.dumps({command: 'fail'}))
            def render():
                result = subprocess.run(['python3', str(root / 'scripts/pr/comment/new-findings.py')], env=env, check=True, capture_output=True, text=True)
                return (current / 'new-failures.md').read_text(), result.stdout
            body, annotations = render()
            if baseline_status == 'pass':
                assert verdict['introduced_failure'] is True, verdict
                assert f'- New failure: `{command}`' in body, body
                assert '@\u200bsomeone' in body and '@someone' not in body
                assert "compile error @\u200bsomeone '''" in body
                assert body.count('```') == 2, body
                assert 'old 2\n' not in body and 'old 3\n' in body
                assert '[Full report and artifacts]' in body
                assert annotations == f'::error title={command}::New failure introduced by this PR\n'
                # Even a stale introduced flag must not bypass an inconclusive verdict.
                verdict['status'] = 'inconclusive'
                (current / 'differential-comment.json').write_text(json.dumps({'commands': {command: verdict}}))
                assert render() == ('', '')
            else:
                assert 'introduced_failure' not in verdict
                assert (body, annotations) == ('', ''), (body, annotations)
    # Explicit baseline_red is never rendered, even with positive raw counts.
    (current / 'differential-comment.json').write_text(json.dumps({'commands': {'build': {'status': 'baseline_red', 'new_findings': 2}}}))
    assert render() == ('', '')
print('PASS: unattributed introduced failures')
PY
