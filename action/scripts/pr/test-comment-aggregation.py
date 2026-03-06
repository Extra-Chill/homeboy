#!/usr/bin/env python3

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/pr/merge-pr-comment.py"


def run_merge(comments: list[dict[str, object]], section_key: str, section_body: str) -> dict[str, object]:
    with tempfile.TemporaryDirectory() as tmp:
        tmpdir = Path(tmp)
        comments_path = tmpdir / "comments.json"
        section_path = tmpdir / "section.md"
        comments_path.write_text(json.dumps(comments), encoding="utf-8")
        section_path.write_text(section_body, encoding="utf-8")

        output = subprocess.check_output(
            [
                sys.executable,
                str(SCRIPT),
                str(comments_path),
                "CI:homeboy",
                "homeboy",
                section_key,
                str(section_path),
            ],
            text=True,
        )
        return json.loads(output)


def assert_contains(text: str, needle: str) -> None:
    if needle not in text:
        raise AssertionError(f"missing expected text: {needle!r}\n---\n{text}")


def test_merges_split_sections_into_one_comment() -> None:
    comments = [
        {
            "id": 10,
            "body": "\n".join(
                [
                    "<!-- homeboy-action-results:key=CI:homeboy -->",
                    "## Homeboy Results — `homeboy`",
                    "",
                    "<!-- homeboy-action-section:key=lint:start -->",
                    "### Lint\n:white_check_mark: **lint**",
                    "<!-- homeboy-action-section:key=lint:end -->",
                    "",
                    "---",
                    "*[Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1*",
                ]
            ),
        },
        {
            "id": 11,
            "body": "\n".join(
                [
                    "<!-- homeboy-action-results:key=CI:homeboy -->",
                    "## Homeboy Results — `homeboy`",
                    "",
                    "<!-- homeboy-action-section:key=audit:start -->",
                    "### Audit\n:white_check_mark: **audit**",
                    "<!-- homeboy-action-section:key=audit:end -->",
                    "",
                    "---",
                    "*[Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1*",
                ]
            ),
        },
    ]

    result = run_merge(comments, "test", "### Test\n:x: **test**")

    assert result["comment_id"] == "10"
    assert result["delete_ids"] == ["11"]
    body = str(result["body"])
    assert body.index("### Lint") < body.index("### Test") < body.index("### Audit")
    assert_contains(body, "<!-- homeboy-action-section:key=lint:start -->")
    assert_contains(body, "<!-- homeboy-action-section:key=test:start -->")
    assert_contains(body, "<!-- homeboy-action-section:key=audit:start -->")


def test_migrates_legacy_job_comment() -> None:
    comments = [
        {
            "id": 25,
            "body": "\n".join(
                [
                    "<!-- homeboy-action-results -->",
                    "## Homeboy Results — `homeboy`",
                    "",
                    ":white_check_mark: **audit**",
                    "",
                    "---",
                    "*[Homeboy Action](https://github.com/Extra-Chill/homeboy-action) v1 — homeboy 0.59.0*",
                    "",
                    "<!-- homeboy-action-results:audit -->",
                ]
            ),
        }
    ]

    result = run_merge(comments, "test", "### Test\n:x: **test**")

    assert result["comment_id"] == "25"
    body = str(result["body"])
    assert_contains(body, "<!-- homeboy-action-section:key=audit:start -->")
    assert_contains(body, "<!-- homeboy-action-section:key=test:start -->")
    assert_contains(body, ":white_check_mark: **audit**")


def main() -> int:
    test_merges_split_sections_into_one_comment()
    test_migrates_legacy_job_comment()
    print("comment aggregation checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
