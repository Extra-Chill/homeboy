#!/usr/bin/env python3
"""Emit Homeboy compiler warning fix suggestions from Cargo JSON output."""

from __future__ import annotations

import json
import sys
from pathlib import Path

from compiler_warnings_lib import (
    relative_path,
    run_cargo_check,
    span_replaced_text,
    warning_code,
    warning_messages,
)


def fix_kind(code: str, line_start: int, line_end: int, replacement: str) -> str | None:
    if code in {"unused_imports", "dead_code"}:
        return "line_removal"
    if line_start == line_end:
        return "line_replacement"
    if replacement == "":
        return "line_removal"
    return None


def main() -> int:
    payload = json.load(sys.stdin)
    root = Path(payload.get("root") or ".")
    fixes = []

    for message in warning_messages(run_cargo_check(root)):
        code = warning_code(message)
        text = str(message.get("message") or "")
        for child in message.get("children") or []:
            for span in child.get("spans") or []:
                if "suggested_replacement" not in span:
                    continue

                file = relative_path(root, str(span.get("file_name") or ""))
                if not file or file.startswith("/") or "/.cargo/" in file:
                    continue

                line_start = int(span.get("line_start") or 1)
                line_end = int(span.get("line_end") or line_start)
                replacement = str(span.get("suggested_replacement") or "")
                kind = fix_kind(code, line_start, line_end, replacement)
                if kind is None:
                    continue

                fixes.append(
                    {
                        "code": code,
                        "kind": kind,
                        "file": file,
                        "line_start": line_start,
                        "line_end": line_end,
                        "original_text": span_replaced_text(span),
                        "replacement": replacement,
                        "message": text,
                    }
                )

    fixes.sort(key=lambda fix: (fix["file"], fix["line_start"], fix["code"]))
    deduped = []
    seen = set()
    for fix in fixes:
        key = (fix["file"], fix["line_start"], fix["code"])
        if key in seen:
            continue
        seen.add(key)
        deduped.append(fix)

    print(json.dumps({"fixes": deduped}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
