#!/usr/bin/env bash
# Inventory requests must bind to explicit components, while all transport and
# downstream readers keep the historical workspace-root inventory path.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "${1:-${ROOT_DIR}/.github/workflows/ci.yml}" <<'PY'
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import yaml

workflow = yaml.safe_load(Path(sys.argv[1]).read_text())
root_inventory = "${{ github.workspace }}/homeboy-test-inventory.json"
request = "${{ github.workspace }}/${{ inputs.component != '' && format('{0}/', inputs.component) || '' }}homeboy-test-inventory.json"

for job, producer, copy_name, consumer, consumer_var in [
    ('baseline', 'Enumerate base Test inventory without running tests',
     'Copy base Test inventory to workspace root',
     'Materialize base-bound candidate Test selection', 'BASELINE_INVENTORY_FILE'),
    ('candidate-test-plan', 'Configure candidate Test shards',
     'Copy candidate Test inventory to workspace root',
     'Plan candidate Test shards', 'TEST_INVENTORY_FILE'),
]:
    steps = workflow['jobs'][job]['steps']
    by_name = {step.get('name'): step for step in steps}
    enumerate_step = by_name[producer]
    assert enumerate_step['env']['HOMEBOY_TEST_INVENTORY_FILE'] == request, job
    assert enumerate_step['env']['HOMEBOY_TEST_INVENTORY_ONLY'] == '1'
    assert enumerate_step['with']['component'] == '${{ inputs.component }}'
    copy = by_name[copy_name]
    assert steps.index(enumerate_step) < steps.index(copy) < steps.index(by_name[consumer])
    assert "inputs.component != ''" in copy['if']  # root callers do not self-copy
    assert by_name[consumer]['env'][consumer_var] == root_inventory
    if job == 'candidate-test-plan':
        assert "steps.inventory.outcome == 'success'" in copy['if']
        assert "steps.copy-inventory.outcome != 'failure'" in by_name[consumer]['if']
        assert copy['continue-on-error'] is True  # failed copy reaches provenance
    else:
        assert "steps.baseline-selection-policy.outputs.runner != ''" in copy['if']

    # Exercise the actual workflow copy command, including a path with spaces.
    for component in ('', 'plugins/data-machine', 'plugins/with spaces'):
        with tempfile.TemporaryDirectory() as temp:
            workspace = Path(temp)
            source = workspace / component / 'homeboy-test-inventory.json'
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text('{"tests": ["component-specific"]}\n')
            resolved_request = request.replace('${{ github.workspace }}', temp).replace(
                "${{ inputs.component != '' && format('{0}/', inputs.component) || '' }}",
                component + '/' if component else '')
            assert Path(resolved_request) == source
            if component:
                resolved_source = copy['env']['INVENTORY_SOURCE'].replace(
                    '${{ github.workspace }}', temp).replace('${{ inputs.component }}', component)
                subprocess.run(['bash', '-euc', copy['run']], check=True,
                               env={**os.environ, 'GITHUB_WORKSPACE': temp,
                                    'INVENTORY_SOURCE': resolved_source})
            assert (workspace / 'homeboy-test-inventory.json').read_text() == source.read_text()

# No downstream reader or artifact path needs to know the component layout.
plan_steps = workflow['jobs']['candidate-test-plan']['steps']
upload = next(s for s in plan_steps if s.get('name') == 'Upload candidate Test shard plan')
assert 'homeboy-test-inventory.json' in upload['with']['path'].splitlines()
for job in workflow['jobs'].values():
    for step in job.get('steps', []):
        for key, value in step.get('env', {}).items():
            if key in ('BASELINE_INVENTORY_FILE', 'TEST_INVENTORY_FILE'):
                assert value in (root_inventory, 'homeboy-test-inventory.json'), (key, value)
print('PASS: component inventories normalize before all root-level consumers')
PY
