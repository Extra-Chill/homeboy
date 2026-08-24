#!/usr/bin/env python3
"""Exercise the bounded, redacted Test identity sidecar contract."""

from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).parent
GENERATOR = ROOT / "generate-test-failure-identities.py"


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        result = root / "result.json"
        evidence = root / "evidence.json"
        secret = "test::internal-name-and-message"
        result.write_text(
            json.dumps(
                {
                    "data": {
                        "findings": [{"fingerprint": secret}],
                        "test_counts": {"failed": 1, "errors": 0},
                    }
                }
            ),
            encoding="utf-8",
        )
        subprocess.run(["python3", str(GENERATOR), "review test", str(result), str(evidence)], check=True)
        payload = json.loads(evidence.read_text(encoding="utf-8"))
        assert payload["schema"] == "homeboy/test-failure-identities/v1"
        assert payload["command"] == "review test"
        assert payload["identities"] and secret not in evidence.read_text(encoding="utf-8")

        result.write_text(
            json.dumps({"data": {"findings": [{"fingerprint": f"test::{index}"} for index in range(300)]}}),
            encoding="utf-8",
        )
        subprocess.run(["python3", str(GENERATOR), "review test", str(result), str(evidence)], check=True)
        payload = json.loads(evidence.read_text(encoding="utf-8"))
        assert len(payload["identities"]) == 256
        assert payload["truncated"] is True

        result.write_text(
            json.dumps(
                {
                    "data": {
                        "findings": [
                            {
                                "fingerprint": "test::::provider test results::test_failure::118 test failure(s) reported",
                                "metadata": {"test_name": "provider test results"},
                                "source": {"label": "test-failures"},
                            }
                        ],
                        "test_counts": {"failed": 118, "errors": 0},
                    }
                }
            ),
            encoding="utf-8",
        )
        subprocess.run(["python3", str(GENERATOR), "review test", str(result), str(evidence)], check=True)
        payload = json.loads(evidence.read_text(encoding="utf-8"))
        assert payload["identities"] == []
        assert payload["truncated"] is False

    print("PASS: test failure identity sidecars are redacted and bounded")


if __name__ == "__main__":
    main()
