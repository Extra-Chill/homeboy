#!/usr/bin/env python3

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any


ROOT_KEY_PATTERN = re.compile(r"<!-- homeboy-action-results:key=(.*?) -->")
LEGACY_ROOT_MARKER = "<!-- homeboy-action-results -->"
LEGACY_JOB_PATTERN = re.compile(r"<!-- homeboy-action-results:([a-zA-Z0-9_.-]+) -->")
SECTION_PATTERN = re.compile(
    r"<!-- homeboy-action-section:key=(.*?):start -->\n?(.*?)\n?<!-- homeboy-action-section:key=\1:end -->",
    re.S,
)


def normalize_text(text: str) -> str:
    return text.strip("\n")


def matches_comment_key(body: str, comment_key: str) -> bool:
    explicit = ROOT_KEY_PATTERN.search(body)
    if explicit:
        return explicit.group(1) == comment_key

    return LEGACY_ROOT_MARKER in body


def infer_legacy_section_key(body: str) -> str:
    marker = LEGACY_JOB_PATTERN.search(body)
    if marker:
        return marker.group(1)
    return "legacy"


def section_sort_key(section_key: str) -> tuple[int, str]:
    normalized = section_key.strip().lower()
    rank_map = {
        "lint": 10,
        "build": 15,
        "test": 20,
        "audit": 30,
        "legacy": 90,
    }
    if normalized in rank_map:
        return rank_map[normalized], normalized

    if "lint" in normalized:
        return 10, normalized
    if "test" in normalized:
        return 20, normalized
    if "audit" in normalized:
        return 30, normalized
    return 50, normalized


def extract_sections(body: str) -> dict[str, str]:
    sections: dict[str, str] = {}

    for match in SECTION_PATTERN.finditer(body):
        key = match.group(1).strip()
        content = normalize_text(match.group(2))
        if key and content:
            sections[key] = content

    if sections:
        return sections

    if LEGACY_ROOT_MARKER in body:
        legacy_key = infer_legacy_section_key(body)
        legacy_body = body
        legacy_body = ROOT_KEY_PATTERN.sub("", legacy_body)
        legacy_body = legacy_body.replace(LEGACY_ROOT_MARKER, "")
        legacy_body = LEGACY_JOB_PATTERN.sub("", legacy_body)
        legacy_body = normalize_text(legacy_body)
        if legacy_body:
            sections[legacy_key] = legacy_body

    return sections


def build_comment_body(
    comment_key: str,
    component_id: str,
    sections: dict[str, str],
    tooling: dict[str, str] | None = None,
) -> str:
    lines: list[str] = []
    lines.append(f"<!-- homeboy-action-results:key={comment_key} -->")
    lines.append(f"## Homeboy Results — `{component_id}`")
    lines.append("")

    ordered_keys = sorted(sections.keys(), key=section_sort_key)
    for idx, section_key in enumerate(ordered_keys):
        lines.append(f"<!-- homeboy-action-section:key={section_key}:start -->")
        lines.append(normalize_text(sections[section_key]))
        lines.append(f"<!-- homeboy-action-section:key={section_key}:end -->")
        if idx != len(ordered_keys) - 1:
            lines.append("")

    if tooling:
        lines.append("")
        lines.append("<details><summary>Tooling versions</summary>")
        lines.append("")
        lines.append(f"- Homeboy CLI: `{tooling.get('homeboy_cli_version', 'unknown')}`")
        lines.append(
            f"- Extension: `{tooling.get('extension_id', 'auto')}` "
            f"from `{tooling.get('extension_source', 'auto')}`"
        )
        lines.append(f"- Extension revision: `{tooling.get('extension_revision', 'unknown')}`")
        lines.append(
            f"- Action: `{tooling.get('action_repository', 'unknown')}"
            f"@{tooling.get('action_ref', 'unknown')}`"
        )
        lines.append("")
        lines.append("</details>")

    lines.append("")
    lines.append("---")
    lines.append("*[Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1*")
    return "\n".join(lines).strip() + "\n"


def main() -> int:
    if len(sys.argv) not in (6, 7):
        print(
            "Usage: merge-pr-comment.py <comments-json> <comment-key> <component-id> <section-key> <section-file> [tooling-json]",
            file=sys.stderr,
        )
        return 1

    comments_path = Path(sys.argv[1])
    comment_key = sys.argv[2]
    component_id = sys.argv[3]
    section_key = sys.argv[4]
    section_file = Path(sys.argv[5])
    tooling_file = Path(sys.argv[6]) if len(sys.argv) > 6 else None

    tooling: dict[str, str] | None = None
    if tooling_file and tooling_file.is_file():
        try:
            tooling = json.loads(tooling_file.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            tooling = None

    comments: list[dict[str, Any]] = json.loads(comments_path.read_text(encoding="utf-8"))
    current_section = normalize_text(section_file.read_text(encoding="utf-8", errors="replace"))

    matching_comments = [comment for comment in comments if matches_comment_key(str(comment.get("body", "")), comment_key)]

    matching_comments.sort(key=lambda comment: int(comment.get("id", 0)))

    merged_sections: dict[str, str] = {}
    for comment in matching_comments:
        body = str(comment.get("body", ""))
        merged_sections.update(extract_sections(body))

    merged_sections[section_key] = current_section

    canonical_id = str(matching_comments[0]["id"]) if matching_comments else ""
    delete_ids = [str(comment["id"]) for comment in matching_comments[1:]]

    payload = {
        "comment_id": canonical_id,
        "delete_ids": delete_ids,
        "body": build_comment_body(comment_key, component_id, merged_sections, tooling),
    }
    print(json.dumps(payload))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
