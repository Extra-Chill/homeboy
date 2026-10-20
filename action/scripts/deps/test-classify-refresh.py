#!/usr/bin/env python3
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("refresh", Path(__file__).with_name("classify-refresh.py"))
refresh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh)


def report(breaking=False, state="updated"):
    return {"breaking": breaking, "edges": [{"package": "example", "from": "1.0.0", "to": "2.0.0", "state": state, "breaking": breaking, "breaking_reasons": ["Major release"]}]}


class RefreshTests(unittest.TestCase):
    def test_routes(self):
        opted = {"release": {"refresh_dependencies": True}}
        self.assertEqual(refresh.classify(report(state="current"), opted), "none")
        self.assertEqual(refresh.classify({"breaking": False, "edges": []}, opted), "none")
        self.assertEqual(refresh.classify(report(), opted), "release")
        self.assertEqual(refresh.classify(report(), {}), "pr")
        for verdict in (True, None):
            self.assertEqual(refresh.classify(report(verdict), opted), "pr")
            mixed = report(False)
            mixed["edges"].append(report(verdict)["edges"][0])
            self.assertEqual(refresh.classify(mixed, opted), "pr")
        wrapped = {"success": True, "data": report()}
        self.assertEqual(refresh.classify(wrapped, opted), "release")

    def test_fail_closed(self):
        for state in ("failed", "unresolvable", "held", "bogus"):
            with self.assertRaises(ValueError):
                refresh.classify(report(state=state), {})
        with self.assertRaises(ValueError):
            refresh.classify({"success": False, "data": report()}, {})
        malformed = report()
        del malformed["edges"][0]["breaking"]
        with self.assertRaises(ValueError):
            refresh.classify(malformed, {})

    def test_cli_and_body(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "report").write_text(json.dumps(report(True)))
            (path / "config").write_text('{}')
            result = subprocess.run(["python3", str(ROOT / "scripts/deps/classify-refresh.py"), str(path / "report"), str(path / "config"), str(path / "body")], check=True, capture_output=True, text=True)
            self.assertEqual(result.stdout, "route=pr\n")
            body = (path / "body").read_text()
            for text in ("example", "1.0.0", "2.0.0", "Major release"):
                self.assertIn(text, body)

    def test_workflow_contract(self):
        workflow = (ROOT / ".github/workflows/deps-refresh.yml").read_text()
        for text in ("workflow_call:", "group: release", "cancel-in-progress: false", "startsWith(github.event.head_commit.message, 'release:')", "actions/create-github-app-token@v3", "persist-credentials: false", "steps.app-token.outputs.token || github.token", "homeboy deps stack refresh --dry-run", "commands: review lint,review test", "uses: ./.github/workflows/release.yml", "secrets: inherit", "force-with-lease"):
            source = workflow if text != "force-with-lease" else (ROOT / "scripts/deps/upsert-pr.sh").read_text()
            self.assertIn(text, source)
        release = (ROOT / ".github/workflows/release.yml").read_text()
        start = "      - name: Generate GitHub App token"
        end = "      - uses: actions/checkout@v6"
        self.assertIn(release[release.index(start):release.index(end, release.index(start))], workflow)


if __name__ == "__main__":
    unittest.main()
