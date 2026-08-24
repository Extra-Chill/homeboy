#!/usr/bin/env python3
"""Write bounded, redacted per-test failure identities for CI comparison."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path
from typing import Any

SCHEMA = "homeboy/test-failure-identities/v1"
MAX_IDENTITIES = 256


def unwrap(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        return {}
    data = payload.get("data")
    return data if isinstance(data, dict) else payload


def text(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


def fingerprint(item: Any) -> str | None:
    if not isinstance(item, dict):
        return None
    source = item.get("source") if isinstance(item.get("source"), dict) else {}
    metadata = item.get("metadata") if isinstance(item.get("metadata"), dict) else {}
    # Homeboy emits this aggregate when its per-test sidecar is malformed. Its
    # fingerprint includes a variable count, so it is not a test identity.
    if source.get("label") == "test-failures" and text(metadata.get("test_name")) == "provider test results":
        return None
    for key in ("fingerprint", "id"):
        value = text(item.get(key))
        if value:
            return f"{key}:{value}"
    name = text(metadata.get("test_name")) or text(item.get("test_name")) or text(item.get("name"))
    suite = text(metadata.get("suite")) or text(item.get("suite"))
    if name:
        return f"test:{suite}:{name}"
    return None


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: test-failure-identities.py COMMAND RESULT_JSON OUTPUT_JSON", file=sys.stderr)
        return 2
    command, result_path, output_path = sys.argv[1:]
    try:
        payload = json.loads(Path(result_path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        print(f"cannot read test result for identity evidence: {error}", file=sys.stderr)
        return 1

    data = unwrap(payload)
    raw: set[str] = set()
    for key in ("findings", "test_failures", "failures", "errors"):
        items = data.get(key)
        if isinstance(items, list):
            raw.update(value for item in items if (value := fingerprint(item)))
    digests = sorted(hashlib.sha256(value.encode("utf-8")).hexdigest() for value in raw)
    truncated = len(digests) > MAX_IDENTITIES
    evidence = {
        "schema": SCHEMA,
        "command": command,
        "identities": digests[:MAX_IDENTITIES],
        "truncated": truncated,
    }
    Path(output_path).write_text(json.dumps(evidence, separators=(",", ":")) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
