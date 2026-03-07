#!/usr/bin/env python3
"""Build a GitHub PR review payload from Homeboy annotation sidecar JSON files.

Usage:
    build-review.py <annotations_dir> <changed_files_path> <commit_sha>

Reads all *.json files from <annotations_dir> (written by homeboy extension
lint scripts), filters to annotations in changed files only, and prints a
JSON payload suitable for the GitHub Pull Request Reviews API.

Collateral damage (effects on files referencing changed symbols) is handled
by the audit engine via --changed-since, not by the review layer.

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


def main():
    if len(sys.argv) != 4:
        print(
            f"Usage: {sys.argv[0]} <annotations_dir> <changed_files_path> "
            f"<commit_sha>",
            file=sys.stderr,
        )
        sys.exit(1)

    annotations_dir = sys.argv[1]
    changed_files_path = sys.argv[2]
    commit_sha = sys.argv[3]

    # Load inputs
    changed_files = load_file_set(changed_files_path)
    all_annotations = load_annotations(annotations_dir)

    if not all_annotations:
        sys.exit(0)

    # Filter to annotations in changed files only
    inline_comments = []
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

    if not inline_comments:
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
    body = f'Homeboy found **{len(inline_comments)}** issue(s) in changed files.'
    if overflow_note:
        body += overflow_note

    payload = {
        'commit_id': commit_sha,
        'body': body,
        'event': 'COMMENT',
        'comments': inline_comments,
    }

    print(json.dumps(payload))


if __name__ == '__main__':
    main()
