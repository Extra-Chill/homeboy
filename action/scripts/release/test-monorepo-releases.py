#!/usr/bin/env python3
"""Offline execution of the generated serial step contract."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('run-monorepo-releases.py').resolve()
spec = importlib.util.spec_from_file_location('monorepo', SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class MonorepoTests(unittest.TestCase):
    def test_serial_release_contract(self):
        old = Path.cwd()
        with tempfile.TemporaryDirectory() as tmp:
            os.chdir(tmp)
            try:
                subprocess.run(['git', 'init', '-q'], check=True)
                for directory in ('c-later', 'a-skip', 'b-fail'):
                    Path(directory).mkdir()
                    Path(directory, 'homeboy.json').write_text(json.dumps({'id': directory}))
                subprocess.run(['git', 'add', '.'], check=True)
                Path('untracked').mkdir()
                Path('untracked/homeboy.json').write_text('{"id":"ignored"}')
                env = dict(os.environ, RELEASE_INPUTS=json.dumps({'components': 'all'}),
                           GITHUB_OUTPUT=str(Path(tmp, 'output')), RUNNER_TEMP=tmp)
                subprocess.run(['python3', str(SCRIPT), 'generate'], env=env, check=True)
                action = json.loads(Path('.homeboy-monorepo/action.yml').read_text())
                release_steps = [s for s in action['runs']['steps'] if 'uses' in s]
                self.assertEqual([s['with']['component'] for s in release_steps],
                                 ['a-skip', 'b-fail', 'c-later'])
                for step in release_steps:
                    self.assertTrue(step['continue-on-error'])
                    self.assertEqual(step['uses'], './.homeboy-action')
                    self.assertEqual(step['with']['commands'], 'release')
                    self.assertEqual(step['with']['release-skip-publish'], '')
                # Simulate Actions executing the interleaved record steps. The
                # final successful release must still be recorded after failure.
                for index, outcome in enumerate(('success', 'failure', 'success')):
                    subprocess.run(['python3', str(SCRIPT), 'record'], check=True,
                                   env=dict(env, COMPONENT_INDEX=str(index),
                                            COMPONENT_ID=release_steps[index]['with']['component'],
                                            OUTCOME=outcome, RELEASED=str(index == 2).lower(),
                                            RELEASE_VERSION='1.2.3' if index == 2 else '',
                                            RELEASE_TAG='c-later-v1.2.3' if index == 2 else '',
                                            SKIPPED_REASON='no-releasable-commits' if index == 0 else ''))
                completed = subprocess.run(['python3', str(SCRIPT), 'finish'], env=env)
                self.assertEqual(completed.returncode, 1)
                rows = json.loads(Path(tmp, 'homeboy-monorepo-releases.json').read_text())
                self.assertEqual(rows[0]['skipped_reason'], 'no-releasable-commits')
                self.assertEqual(rows[1]['skipped_reason'], 'release-failed')
                self.assertTrue(rows[2]['released'])
                # A no-op alone is successful.
                Path('.homeboy-monorepo/components.json').write_text(json.dumps([{'component': 'a-skip'}]))
                self.assertEqual(subprocess.run(['python3', str(SCRIPT), 'finish'], env=env).returncode, 0)
            finally:
                os.chdir(old)

    def test_invalid_mode(self):
        with tempfile.TemporaryDirectory() as tmp:
            for inputs in ({'components': ''}, {'components': 'changed'},
                           {'components': 'all', 'component': 'one'}):
                result = subprocess.run(['python3', str(SCRIPT), 'generate'], cwd=tmp,
                                        env=dict(os.environ, RELEASE_INPUTS=json.dumps(inputs)),
                                        capture_output=True)
                self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
