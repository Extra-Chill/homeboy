#!/usr/bin/env python3
"""Emit Homeboy compiler warning findings from Cargo JSON output."""

from __future__ import annotations

import json
import sys
from pathlib import Path

from compiler_warnings_lib import (
    primary_span,
    relative_path,
    run_cargo_check,
    warning_code,
    warning_messages,
    warning_suggestion,
)


def main() -> int:
    payload = json.load(sys.stdin)
    root = Path(payload.get("root") or ".")
    warnings = []

    for message in warning_messages(run_cargo_check(root)):
        code = warning_code(message)
        text = str(message.get("message") or "")
        span = primary_span(message)
        if span is None:
            continue

        file = relative_path(root, str(span.get("file_name") or ""))
        if not file or file.startswith("/") or "/.cargo/" in file:
            continue
        if code == "unknown" and "generated" in text:
            continue

        warnings.append(
            {
                "code": code,
                "message": text,
                "file": file,
                "line": int(span.get("line_start") or 0),
                "suggestion": warning_suggestion(code),
            }
        )

    warnings.sort(key=lambda warning: (warning["file"], warning["line"], warning["code"]))
    deduped = []
    seen = set()
    for warning in warnings:
        key = (warning["file"], warning["line"], warning["code"])
        if key in seen:
            continue
        seen.add(key)
        deduped.append(warning)

    print(json.dumps({"warnings": deduped}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
