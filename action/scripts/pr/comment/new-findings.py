#!/usr/bin/env python3
"""Render only attributed failures and workflow annotations (never raw totals)."""
import json
import os
from pathlib import Path
import subprocess


def load(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return {}


def safe(value):
    return str(value).replace('@', '@\u200b')


def escape(value, prop=False):
    value = safe(value).replace('%', '%25').replace('\r', '%0D').replace('\n', '%0A')
    return value.replace(':', '%3A').replace(',', '%2C') if prop else value


def code(value):
    return '`' + safe(value).replace('`', "'").replace('\r', ' ').replace('\n', ' ') + '`'


def main():
    root = Path(os.environ['OUTPUT_DIR'])
    verdicts = load(root / 'differential-comment.json').get('commands', {})
    results = json.loads(os.environ.get('RESULTS', '{}'))
    findings, tests, failures = [], [], []
    changed = None
    for command in os.environ.get('COMMANDS', '').split(','):
        command = command.strip()
        verdict = verdicts.get(command, {})
        status = verdict.get('status', results.get(command))
        if status not in ('fail', 'timeout'):
            continue
        details = verdict.get('new_finding_details')
        if details is None and command in ('lint', 'review lint'):
            # Without a baseline set, limit attribution to PR-changed files.
            if changed is None:
                try:
                    output = subprocess.check_output([
                        'gh', 'api', '--paginate',
                        f"repos/{os.environ['GITHUB_REPOSITORY']}/pulls/{os.environ['PR_NUMBER']}/files",
                        '--jq', '.[].filename'], text=True, stderr=subprocess.DEVNULL)
                    changed = set(output.splitlines())
                except (OSError, subprocess.CalledProcessError, KeyError):
                    changed = set()
            stem = command.replace(' ', '-')
            payload = load(root / (stem + '.json'))
            data = payload.get('data', payload)
            details = [f for f in data.get('lint_findings', [])
                       if isinstance(f, dict) and f.get('file') in changed]
        findings.extend(details or [])
        tests.extend(verdict.get('new_failed_tests', []))
        if not details and not verdict.get('new_failed_tests') and (verdict.get('new_findings') or 0) > 0:
            failures.append(command)

    ranks = {'critical': 0, 'error': 1, 'high': 1, 'warning': 2, 'medium': 2, 'info': 3, 'low': 3}
    findings.sort(key=lambda f: ranks.get(str(f.get('severity', 'error')).lower(), 1))
    unique = {}
    for f in findings:
        unique.setdefault(f.get('fingerprint') or json.dumps(f, sort_keys=True), f)
    findings = list(unique.values())
    lines = []
    for f in findings[:10]:
        lines.append('- ' + code(f"{f.get('file', '?')}:{f.get('line') or 1} — {f.get('rule', 'finding')} — {f.get('message', '')}"))
    if len(findings) > 10:
        lines.append('- Additional new findings are in the run summary.')
    lines.extend('- Failing test: ' + code(t) for t in sorted(set(tests)))
    lines.extend('- New failures: ' + code(c) for c in failures)
    if lines:
        url = f"{os.environ.get('GITHUB_SERVER_URL', 'https://github.com')}/{os.environ.get('GITHUB_REPOSITORY', '')}/actions/runs/{os.environ.get('GITHUB_RUN_ID', '')}"
        lines.append(f'\n[Full report and artifacts]({url})')
    (root / 'new-failures.md').write_text('\n'.join(lines))
    emitted = 0
    for f in findings:
        if emitted >= 50:
            break
        if not f.get('file'):
            continue
        level = 'warning' if ranks.get(str(f.get('severity', '')).lower(), 1) >= 2 else 'error'
        line = f.get('line') or 1
        if not isinstance(line, int) or line < 1:
            line = 1
        print(f"::{level} file={escape(f['file'], True)},line={line},title={escape(f.get('rule', 'finding'), True)}::{escape(f.get('message', ''))}")
        emitted += 1


if __name__ == '__main__':
    main()
