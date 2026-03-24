#!/usr/bin/env python3
"""Smoke test for audit comment rendering from structured JSON output.

Requires a real audit log fixture at the hardcoded path — skips if unavailable.
Tests the digest markdown generation and render-command-summary.py output.
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
REAL_LOG = Path("/root/.local/share/opencode/tool-output/tool_cc14c67d8001kLeyRdnG0KabT0")


def assert_contains(text: str, needle: str) -> None:
    if needle not in text:
        raise AssertionError(f"missing expected text: {needle!r}\n---\n{text}")


def main() -> int:
    if not REAL_LOG.is_file():
        print("skipping: real audit log fixture not available")
        return 0

    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        audit_log = tmpdir / "audit.log"
        audit_log.write_text(REAL_LOG.read_text(encoding="utf-8", errors="replace"), encoding="utf-8")

        digest_md = subprocess.check_output(
            [
                sys.executable,
                str(ROOT / "scripts/digest/build-failure-digest.py"),
                str(tmpdir),
                json.dumps({"audit": "fail"}),
                "https://github.com/Extra-Chill/homeboy/actions/runs/22747992649",
                "homeboy 0.59.0",
                "rust",
                "https://github.com/Extra-Chill/homeboy-extensions",
                "unknown",
                "Extra-Chill/homeboy-action",
                "v1",
                "audit",
                "false",
                "false",
                "",
            ],
            text=True,
        ).strip()

        digest_text = Path(digest_md).read_text(encoding="utf-8")

        assert_contains(digest_text, "### Audit Failure Digest")
        assert_contains(digest_text, "- Human-needed failed commands:")

        # Test render-command-summary.py reads the raw --output JSON directly
        audit_json_path = tmpdir / "audit.json"
        if audit_json_path.is_file():
            audit_summary = subprocess.check_output(
                [
                    sys.executable,
                    str(ROOT / "scripts/digest/render-command-summary.py"),
                    "audit",
                    str(audit_json_path),
                    "markdown",
                ],
                text=True,
            )
            # Basic sanity — should produce some markdown output
            assert len(audit_summary.strip()) > 0, "audit markdown summary should not be empty"

    print("audit comment rendering checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
