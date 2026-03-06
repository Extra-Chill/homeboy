#!/usr/bin/env python3
"""Render actionable markdown from Homeboy audit JSON output in a log file.

Reads a Homeboy command log, extracts the last parseable JSON object that looks
like an audit payload, and prints concise markdown suitable for PR comments.
"""

from __future__ import annotations

import json
import sys
from json import JSONDecoder


def extract_json_candidates(text: str) -> list[dict]:
    decoder = JSONDecoder()
    out: list[dict] = []
    i = 0

    while i < len(text):
        if text[i] != "{":
            i += 1
            continue

        try:
            obj, end = decoder.raw_decode(text[i:])
        except json.JSONDecodeError:
            i += 1
            continue

        if isinstance(obj, dict):
            out.append(obj)

        i += max(end, 1)

    return out


def normalize_log_text(raw_text: str) -> str:
    """Strip GitHub log prefixes so multiline JSON can be reconstructed."""
    normalized_lines: list[str] = []
    for line in raw_text.splitlines():
        if "Z " in line:
            # GitHub log lines usually end the timestamp with `Z `.
            normalized_lines.append(line.rsplit("Z ", 1)[1])
        else:
            normalized_lines.append(line)
    return "\n".join(normalized_lines)


def is_audit_payload(obj: dict) -> bool:
    data = obj.get("data")
    if not isinstance(data, dict):
        return False

    if "baseline_comparison" in data:
        return True

    command = str(data.get("command", ""))
    return command.startswith("audit")


def main() -> int:
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <audit-log-file>", file=sys.stderr)
        return 1

    log_path = sys.argv[1]
    try:
        with open(log_path, "r", encoding="utf-8", errors="replace") as f:
            text = f.read()
    except OSError:
        return 1

    payloads = [
        p for p in extract_json_candidates(normalize_log_text(text)) if is_audit_payload(p)
    ]
    if not payloads:
        return 2

    payload = payloads[-1]
    data = payload.get("data", {})
    baseline = data.get("baseline_comparison", {}) if isinstance(data, dict) else {}

    drift_increased = bool(baseline.get("drift_increased", False))
    new_items = baseline.get("new_items", [])
    if not isinstance(new_items, list):
        new_items = []

    resolved = baseline.get("resolved_fingerprints", [])
    if not isinstance(resolved, list):
        resolved = []

    summary = data.get("summary", {}) if isinstance(data, dict) else {}
    outliers_found = summary.get("outliers_found") if isinstance(summary, dict) else None

    if drift_increased:
        print(f"- Drift increased: **{len(new_items)}** new finding(s)")
    else:
        print("- Drift increased: **no**")

    if isinstance(outliers_found, int):
        print(f"- Outliers in current run: **{outliers_found}**")

    if resolved:
        print(f"- Resolved findings since baseline: **{len(resolved)}**")

    if new_items:
        print("\n<details><summary>New findings (actionable)</summary>\n")
        for idx, item in enumerate(new_items[:10], start=1):
            if not isinstance(item, dict):
                continue
            context = item.get("context_label", "unknown")
            desc = item.get("description", "(no description)")
            fp = item.get("fingerprint", "")
            fp_part = f" (`{fp}`)" if fp else ""
            print(f"{idx}. **{context}** — {desc}{fp_part}")

        if len(new_items) > 10:
            print(f"\n_...and {len(new_items) - 10} more new finding(s)._")

        print("\n</details>")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
