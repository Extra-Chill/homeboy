#!/usr/bin/env python3
"""Build the CI component matrix; detection errors must never become empty success."""
import json
import os
from pathlib import Path
import subprocess


def git(*args):
    return subprocess.check_output(['git', *args], text=True).strip()


def resolve_base(base, head, event):
    if not base:
        raise ValueError('components: changed requires a PR base or push before SHA; use all for dispatch/schedule')
    if event in ('pull_request', 'pull_request_target'):
        # The detector checkout uses fetch-depth: 0, including base ancestry.
        # Fail detection rather than silently comparing against a moving base tip.
        return git('merge-base', base, head)
    if set(base) == {'0'}:
        tree = git('hash-object', '-t', 'tree', '/dev/null')
        return git('-c', 'user.name=CI', '-c', 'user.email=ci@example.com',
                   'commit-tree', tree, '-m', 'Initial empty tree')
    return base


def discover():
    files = subprocess.check_output(
        ['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard']
    ).decode().split('\0')
    rows = []
    for name in sorted(set(files)):
        path = Path(name)
        if path.name == 'homeboy.json' and path.is_file():
            rows.append({'component': json.loads(path.read_text())['id'],
                         'path': path.parent.as_posix()})
    return {'include': rows}


def plan():
    mode = os.environ['COMPONENTS']
    if mode == 'all':
        matrix = discover()
    elif mode == 'changed':
        head = os.environ['HEAD_SHA']
        base = resolve_base(os.environ.get('BASE_SHA', ''), head,
                            os.environ.get('GITHUB_EVENT_NAME', ''))
        command = ['homeboy', 'component', 'changed', '--base', base, '--head', head,
                   '--path', '.', '--format', 'matrix']
        for pattern in os.environ.get('SHARED_PATHS', '').splitlines():
            if pattern.strip():
                command.extend(['--shared-path', pattern.strip()])
        matrix = json.loads(subprocess.check_output(command, text=True))
    else:
        raise ValueError('components must be changed or all')
    if not isinstance(matrix, dict) or not isinstance(matrix.get('include'), list):
        raise ValueError('Invalid component matrix')
    identities = set()
    paths = set()
    for row in matrix['include']:
        identity, path = row['component'], row['path']
        if (not isinstance(identity, str) or not identity or '\n' in identity
                or not isinstance(path, str) or not path or '\n' in path
                or Path(path).is_absolute() or '..' in Path(path).parts
                or not Path(path, 'homeboy.json').is_file()):
            raise ValueError(f'Invalid component entry: {row!r}')
        if identity in identities or path in paths:
            raise ValueError('Duplicate component identity or path')
        if json.loads(Path(path, 'homeboy.json').read_text())['id'] != identity:
            raise ValueError(f'Component identity mismatch: {row!r}')
        identities.add(identity)
        paths.add(path)
    if len(matrix['include']) > 256:
        raise ValueError('GitHub Actions supports at most 256 component jobs')
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write('matrix=' + json.dumps(matrix, separators=(',', ':')) + '\n')
        output.write('enabled=' + str(bool(matrix['include'])).lower() + '\n')


if __name__ == '__main__':
    plan()
