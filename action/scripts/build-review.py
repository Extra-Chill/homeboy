#!/usr/bin/env python3
"""Build a GitHub PR review payload from Homeboy annotation sidecar JSON files.

Usage:
    build-review.py <annotations_dir> <changed_files_path> <commit_sha> [<related_files_path>]

Reads all *.json files from <annotations_dir> (written by homeboy extension
lint scripts), splits annotations into inline (in PR diff) vs collateral
(in untouched files), and prints a JSON payload suitable for the GitHub
Pull Request Reviews API.

Collateral damage is scoped: only errors in files that reference symbols
from the changed files are reported. If <related_files_path> is provided,
only those files are eligible for collateral. If not provided, collateral
damage is skipped entirely (no way to distinguish pre-existing vs new).

Exits 0 with no output if there are no annotations to post.

Annotation JSON format (array of objects):
    [
        {
            "file": "inc/Foo.php",
            "line": 42,
            "message": "Parameter expects int, string given.",
            "source": "phpstan",
            "severity": "error",
            "code": "argument.type"
        }
    ]

Supported sources: phpcs, phpstan, clippy, rustfmt
"""

import json
import os
import sys


def load_annotations(annotations_dir: str) -> list[dict]:
    """Load all annotation JSON files from the directory."""
    annotations = []
    for fname in sorted(os.listdir(annotations_dir)):
        if not fname.endswith('.json'):
            continue
        fpath = os.path.join(annotations_dir, fname)
        try:
            with open(fpath, 'r') as f:
                data = json.load(f)
            if isinstance(data, list):
                annotations.extend(data)
        except (json.JSONDecodeError, IOError) as e:
            print(f"Warning: failed to read {fname}: {e}", file=sys.stderr)
            continue
    return annotations


def load_file_set(path: str) -> set[str]:
    """Load a set of file paths from a newline-delimited file."""
    with open(path, 'r') as f:
        return set(line.strip() for line in f if line.strip())


def format_comment_body(source: str, code: str, severity: str, message: str) -> str:
    """Format a single inline comment body."""
    icon = ':x:' if severity == 'error' else ':warning:'
    body = f'{icon} **{source}**'
    if code:
        body += f' `{code}`'
    body += f'\n{message}'
    return body


def build_collateral_section(collateral: list[dict]) -> list[str]:
    """Build markdown lines for the collateral damage section."""
    lines = []
    lines.append(
        f'\n### Collateral damage ({len(collateral)} issue(s) in related files)\n'
    )
    lines.append(
        'These errors are in files that reference symbols you changed '
        'but were not part of this PR:\n'
    )

    by_file: dict[str, list[dict]] = {}
    for c in collateral:
        by_file.setdefault(c['file'], []).append(c)

    for fpath, issues in sorted(by_file.items()):
        lines.append(f'**{fpath}**')
        for issue in issues[:5]:
            icon = ':x:' if issue['severity'] == 'error' else ':warning:'
            code_str = f' `{issue["code"]}`' if issue.get('code') else ''
            lines.append(
                f'- {icon} L{issue["line"]}: {issue["message"]}{code_str}'
            )
        if len(issues) > 5:
            lines.append(f'- _...and {len(issues) - 5} more_')
        lines.append('')

    return lines


def main():
    if len(sys.argv) < 4 or len(sys.argv) > 5:
        print(
            f"Usage: {sys.argv[0]} <annotations_dir> <changed_files_path> "
            f"<commit_sha> [<related_files_path>]",
            file=sys.stderr,
        )
        sys.exit(1)

    annotations_dir = sys.argv[1]
    changed_files_path = sys.argv[2]
    commit_sha = sys.argv[3]
    related_files_path = sys.argv[4] if len(sys.argv) > 4 else None

    # Load inputs
    changed_files = load_file_set(changed_files_path)
    all_annotations = load_annotations(annotations_dir)
    related_files = load_file_set(related_files_path) if related_files_path else set()

    if not all_annotations:
        sys.exit(0)

    # Split annotations into three buckets:
    #   1. inline — in changed files (posted as inline PR review comments)
    #   2. collateral — in related files (shown in review body)
    #   3. unrelated — in files with no symbol link to changes (dropped)
    inline_comments = []
    collateral = []
    seen: set[str] = set()

    for ann in all_annotations:
        file_path = ann.get('file', '')
        line = ann.get('line', 0)
        message = ann.get('message', '')
        source = ann.get('source', '')
        code = ann.get('code', '')
        severity = ann.get('severity', 'error')

        if not file_path or not line or not message:
            continue

        dedup_key = f'{file_path}:{line}:{source}:{code}'
        if dedup_key in seen:
            continue
        seen.add(dedup_key)

        if file_path in changed_files:
            inline_comments.append({
                'path': file_path,
                'line': int(line),
                'body': format_comment_body(source, code, severity, message),
            })
        elif file_path in related_files:
            collateral.append({
                'file': file_path,
                'line': int(line),
                'message': message,
                'source': source,
                'code': code,
                'severity': severity,
            })
        # else: unrelated pre-existing error — silently dropped

    # Nothing to post
    if not inline_comments and not collateral:
        sys.exit(0)

    # Cap inline comments at 50 (GitHub API limit per review)
    overflow_note = ''
    if len(inline_comments) > 50:
        overflow = len(inline_comments) - 50
        inline_comments = inline_comments[:50]
        overflow_note = (
            f'\n\n_...and {overflow} more annotations not shown '
            f'(GitHub limits reviews to 50 comments)._'
        )

    # Build review body
    review_body_parts = []
    if inline_comments:
        review_body_parts.append(
            f'Homeboy found **{len(inline_comments)}** issue(s) in changed files.'
        )
    if collateral:
        review_body_parts.extend(build_collateral_section(collateral))
    if overflow_note:
        review_body_parts.append(overflow_note)

    payload = {
        'commit_id': commit_sha,
        'body': '\n'.join(review_body_parts),
        'event': 'COMMENT',
        'comments': inline_comments,
    }

    print(json.dumps(payload))


if __name__ == '__main__':
    main()
