#!/usr/bin/env python3
"""Targeted executable matrix/aggregate tests and workflow wiring contracts."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import yaml

ROOT = Path(__file__).resolve().parents[2]
workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
jobs = workflow['jobs']

with tempfile.TemporaryDirectory() as directory:
    tmp = Path(directory)
    subprocess.run(['git', 'init', '-q', directory], check=True)
    (tmp / 'plugins/example').mkdir(parents=True)
    (tmp / 'bin').mkdir()
    for path, identity in [('.', 'network'), ('plugins/example', 'plugin-id')]:
        (tmp / path / 'homeboy.json').write_text(json.dumps({'id': identity}))
    stub = tmp / 'bin/homeboy'
    stub.write_text('#!/usr/bin/env python3\nimport os,json,sys\n'
                    'open("args.json","w").write(json.dumps(sys.argv[1:]))\n'
                    'print(os.environ["DETECTED_MATRIX"])\n')
    stub.chmod(0o755)
    env = dict(os.environ, PATH=f'{tmp}/bin:' + os.environ['PATH'],
               GITHUB_OUTPUT=str(tmp / 'output'), COMPONENTS='changed',
               BASE_SHA='base', HEAD_SHA='head', SHARED_PATHS='.github/**\ntooling/**')

    def plan(matrix, success=True):
        (tmp / 'output').unlink(missing_ok=True)
        env['DETECTED_MATRIX'] = json.dumps(matrix)
        result = subprocess.run(['python3', str(ROOT / 'scripts/core/plan-components.py')],
                                cwd=tmp, env=env, capture_output=True, text=True)
        assert (result.returncode == 0) == success, result.stderr
        if success:
            return dict(line.split('=', 1) for line in (tmp / 'output').read_text().splitlines())

    matrix = {'include': [{'component': 'plugin-id', 'path': 'plugins/example'},
                          {'component': 'network', 'path': '.'}]}
    output = plan(matrix)
    assert json.loads(output['matrix']) == matrix
    assert output['enabled'] == 'true'
    assert json.loads((tmp / 'args.json').read_text()) == [
        'component', 'changed', '--base', 'base', '--head', 'head', '--path', '.',
        '--format', 'matrix', '--shared-path', '.github/**', '--shared-path', 'tooling/**']
    assert plan({'include': []})['enabled'] == 'false'
    plan({'include': [{'component': 'wrong', 'path': 'plugins/example'}]}, False)
    plan({'include': [{'component': 'network', 'path': '../'}]}, False)
    plan({'include': [matrix['include'][0]] * 2}, False)
    plan({'bogus': []}, False)
    env['BASE_SHA'] = ''
    plan(matrix, False)
    env['BASE_SHA'] = '0' * 40
    plan({'include': []})
    assert json.loads((tmp / 'args.json').read_text())[3] != '0' * 40
    env['COMPONENTS'] = 'all'
    all_rows = json.loads(plan(None)['matrix'])['include']
    assert sorted(all_rows, key=lambda row: row['component']) == sorted(matrix['include'], key=lambda row: row['component'])
    # All mode also works without a root manifest and ignores untracked tooling.
    (tmp / 'homeboy.json').unlink()
    (tmp / '.gitignore').write_text('ignored/\n')
    (tmp / 'ignored').mkdir()
    (tmp / 'ignored/homeboy.json').write_text('{"id":"not-a-target"}')
    assert json.loads(plan(None)['matrix']) == {'include': [matrix['include'][0]]}
    env['COMPONENTS'] = 'invalid'
    plan(matrix, False)

aggregate = jobs['components-result']
assert aggregate['name'] == 'Components'
assert 'always()' in aggregate['if']
assert aggregate['needs'] == ['plan', 'detect-components', 'component-ci']
script = aggregate['steps'][0]['run']
for plan_result, detection, enabled, component, success in [
    ('success', 'success', 'true', 'success', True),
    ('success', 'success', 'false', 'skipped', True),
    ('success', 'success', 'true', 'failure', False),
    ('success', 'success', 'true', 'cancelled', False),
    ('success', 'success', 'true', 'skipped', False),
    ('success', 'failure', '', 'skipped', False),
    ('failure', 'skipped', '', 'skipped', False),
    ('failure', 'success', 'false', 'skipped', False),
    ('success', 'success', 'false', 'failure', False),
]:
    result = subprocess.run(['bash', '-e', '-o', 'pipefail', '-c', script], env=dict(
        os.environ, PLAN_RESULT=plan_result, DETECT_RESULT=detection,
        ENABLED=enabled, COMPONENT_RESULT=component))
    assert (result.returncode == 0) == success

component_job = jobs['component-ci']
assert component_job['strategy']['fail-fast'] is False
assert "outputs.enabled == 'true'" in component_job['if']
step = next(step for step in component_job['steps'] if step.get('uses') == './.homeboy-action')
assert step['with']['component'] == '${{ matrix.path }}'
for name in ['commands', 'extension', 'auto-setup', 'php-version', 'node-version',
             'differential-gating', 'baseline-commands', 'source', 'version']:
    assert step['with'][name] == '${{ inputs.' + name + ' }}'
assert step['with']['pr-policy-merge'] == 'false'
assert '0.436.4' in str(jobs['detect-components'])
detect = next(step for step in jobs['detect-components']['steps'] if step.get('id') == 'detect')
assert detect['env']['BASE_SHA'] == '${{ github.event.pull_request.base.sha || github.event.before }}'
assert any(step.get('id') == 'prepare-detection' for step in jobs['detect-components']['steps'])
assert "inputs.components == ''" in jobs['binary']['if']
planner = jobs['plan']['steps'][-1]
assert planner['env']['COMMANDS'] == "${{ inputs.components == '' && inputs.commands || '' }}"
# Original per-component jobs still consume the original single input.
for job in ['candidate', 'baseline']:
    for step in jobs[job]['steps']:
        if step.get('uses') == './.homeboy-action':
            assert step['with']['component'] == '${{ inputs.component }}'
print('PASS: component matrix, empty matrix, aggregate verdicts, per-directory wiring, and single-component path')
