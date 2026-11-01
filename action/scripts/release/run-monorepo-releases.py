#!/usr/bin/env python3
"""Generate a serial local composite, reusing the complete per-component action.

A matrix cannot share a moving checkout. A generated composite can, while still
using the action's runtime setup, credentials cleanup, publishing and verification.
"""
import json
import os
from pathlib import Path
import subprocess
import sys


def discover():
    names = subprocess.check_output(['git', 'ls-files', '-z']).decode().split('\0')
    rows = []
    identities = set()
    for name in sorted(set(names)):
        path = Path(name)
        if path.name != 'homeboy.json' or not path.is_file():
            continue
        identity = json.loads(path.read_text())['id']
        if not isinstance(identity, str) or not identity or '\n' in identity:
            raise ValueError(f'Invalid component id in {name}')
        if identity in identities:
            raise ValueError(f'Duplicate component id: {identity}')
        identities.add(identity)
        rows.append({'component': identity, 'path': path.parent.as_posix()})
    return rows


def last_tag(identity):
    """The newest <id>-vX.Y.Z tag reachable from HEAD, or None."""
    found = subprocess.run(
        ['git', 'tag', '--merged', 'HEAD', '--list', f'{identity}-v*', '--sort=-v:refname'],
        capture_output=True, text=True)
    tags = found.stdout.split() if found.returncode == 0 else []
    return tags[0] if tags else None


def unchanged_since_release(row):
    """True when nothing under the component's path changed since its last tag.

    Running the full action for such a component only to learn
    no-releasable-commits costs about a minute of setup each, which made a
    no-op monorepo release run take ~28 minutes. Components with no tag yet,
    or with any commit since their tag, still go through Homeboy, which keeps
    the final say on what is releasable.
    """
    tag = last_tag(row['component'])
    if tag is None:
        return False
    log = subprocess.check_output(
        ['git', 'log', '--format=%H', f'{tag}..HEAD', '--', row['path']]).decode()
    return not log.strip()


def generate():
    inputs = json.loads(os.environ['RELEASE_INPUTS'])
    if inputs['components'] != 'all' or inputs.get('component'):
        raise ValueError('Use components: all without a component override')
    directory = Path('.homeboy-monorepo')
    directory.mkdir(exist_ok=True)
    rows = discover()
    (directory / 'components.json').write_text(json.dumps(rows))
    steps = []
    script = 'python3 .homeboy-action/scripts/release/run-monorepo-releases.py'
    for index, row in enumerate(rows):
        if unchanged_since_release(row):
            (directory / f'result-{index}.json').write_text(json.dumps({
                'failed': False,
                'result': {'component': row['component'], 'released': False, 'version': '',
                           'tag': '', 'skipped_reason': 'no-releasable-commits'}}))
            continue
        step_id = f'component_{index}'
        action_inputs = {key: inputs.get(key, '') for key in (
            'args', 'extension', 'extension-ref', 'php-version', 'node-version',
            'execution-timeout-seconds', 'config-dir', 'release-branch',
            'release-skip-publish', 'release-skip-github-release')}
        action_inputs.update({
            'commands': 'release', 'component': row['path'],
            'release-dry-run': inputs.get('dry-run', False),
            'ssh-key': '${{ env.MONOREPO_SSH_KEY }}',
            'ssh-known-hosts': inputs.get('publisher-known-hosts', ''),
            'ssh-require-known-hosts': 'true',
            'npm-token': '${{ env.MONOREPO_NPM_TOKEN }}',
            'app-token': '${{ env.MONOREPO_APP_TOKEN }}',
        })
        # Composite inputs are strings, including booleans from workflow_call.
        action_inputs = {k: str(v).lower() if isinstance(v, bool) else v
                         for k, v in action_inputs.items()}
        steps.append({'id': step_id, 'uses': './.homeboy-action',
                      'continue-on-error': True, 'with': action_inputs})
        env = {'COMPONENT_INDEX': str(index), 'COMPONENT_ID': row['component'],
               'OUTCOME': '${{ steps.' + step_id + '.outcome }}'}
        for key in ('released', 'release-version', 'release-tag', 'skipped-reason'):
            env[key.upper().replace('-', '_')] = '${{ steps.' + step_id + '.outputs.' + key + ' }}'
        steps.append({'name': 'Record ' + row['component'], 'if': '${{ !cancelled() }}',
                      'shell': 'bash', 'env': env, 'run': script + ' record'})
    steps.append({'id': 'aggregate', 'if': '${{ !cancelled() }}', 'shell': 'bash',
                  'run': script + ' finish'})
    action = {'name': 'Serial monorepo releases', 'description': 'Release all tracked components',
              'outputs': {'results': {'description': 'Component release results',
                                     'value': '${{ steps.aggregate.outputs.results }}'}},
              'runs': {'using': 'composite', 'steps': steps}}
    # JSON is YAML, without a third-party serialization dependency.
    (directory / 'action.yml').write_text(json.dumps(action, indent=2))


def record():
    failed = os.environ['OUTCOME'] != 'success'
    result = {'component': os.environ['COMPONENT_ID'],
              'released': not failed and os.environ.get('RELEASED') == 'true',
              'version': os.environ.get('RELEASE_VERSION', ''),
              'tag': os.environ.get('RELEASE_TAG', ''),
              'skipped_reason': ('release-failed' if failed else
                                 os.environ.get('SKIPPED_REASON', ''))}
    Path(f".homeboy-monorepo/result-{os.environ['COMPONENT_INDEX']}.json").write_text(
        json.dumps({'failed': failed, 'result': result}))


def finish():
    rows = json.loads(Path('.homeboy-monorepo/components.json').read_text())
    results, failed = [], False
    for index, row in enumerate(rows):
        path = Path(f'.homeboy-monorepo/result-{index}.json')
        item = json.loads(path.read_text()) if path.exists() else {
            'failed': True, 'result': {'component': row['component'], 'released': False,
                                     'version': '', 'tag': '', 'skipped_reason': 'result-missing'}}
        failed |= item['failed']
        results.append(item['result'])
    payload = json.dumps(results, separators=(',', ':'))
    Path(os.environ['RUNNER_TEMP'], 'homeboy-monorepo-releases.json').write_text(payload)
    with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
        output.write('results=' + payload + '\n')
    print(payload)
    return int(failed)


if __name__ == '__main__':
    sys.exit({'generate': generate, 'record': record, 'finish': finish}[sys.argv[1]]())
