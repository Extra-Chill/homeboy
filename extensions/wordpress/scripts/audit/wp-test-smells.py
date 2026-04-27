#!/usr/bin/env python3
"""WordPress test-smell detector for Homeboy's WordPress extension.

The detector is intentionally narrow: it flags test methods that instantiate
WP_Query and then manually assign result fields that WordPress test fixtures can
populate through query_posts() or a real WP_Query call.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path


SMELL_CODE = "wp.test.mock_over_fixture"
QUERY_FIELDS = ("posts", "post_count", "found_posts")
IGNORE_RE = re.compile(r"homeboy-ignore\s+wp\.test\.mock_over_fixture")
TEST_METHOD_RE = re.compile(
    r"(?P<sig>(?:public|protected|private)?\s*(?:static\s+)?function\s+(?P<name>test\w*)\s*\([^)]*\)\s*)\{",
    re.MULTILINE,
)
NEW_QUERY_RE = re.compile(r"\$(?P<var>\w+)\s*=\s*new\s+\\?WP_Query\s*\(")


@dataclass(frozen=True)
class Finding:
    file: str
    line: int
    method: str
    variable: str
    fields: tuple[str, ...]

    def as_dict(self) -> dict[str, object]:
        return {
            "code": SMELL_CODE,
            "file": self.file,
            "line": self.line,
            "method": self.method,
            "variable": self.variable,
            "fields": list(self.fields),
            "message": message_for(self),
        }


def line_number(content: str, offset: int) -> int:
    return content.count("\n", 0, offset) + 1


def extract_braced_body(content: str, open_brace: int) -> tuple[str, int] | None:
    depth = 0
    in_single = False
    in_double = False
    escape = False

    for pos in range(open_brace, len(content)):
        ch = content[pos]
        if escape:
            escape = False
            continue
        if ch == "\\" and (in_single or in_double):
            escape = True
            continue
        if ch == "'" and not in_double:
            in_single = not in_single
            continue
        if ch == '"' and not in_single:
            in_double = not in_double
            continue
        if in_single or in_double:
            continue
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return content[open_brace + 1 : pos], pos
    return None


def has_ignore(body: str, query_start: int) -> bool:
    prefix = body[max(0, query_start - 500) : query_start]
    return bool(IGNORE_RE.search(prefix) or IGNORE_RE.search(body))


def assigned_query_fields(body: str, var_name: str) -> tuple[str, ...]:
    fields: list[str] = []
    for field in QUERY_FIELDS:
        if re.search(r"\$" + re.escape(var_name) + r"\s*->\s*" + field + r"\s*=", body):
            fields.append(field)
    return tuple(fields)


def scan_file(path: Path, root: Path) -> list[Finding]:
    try:
        content = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        content = path.read_text(encoding="utf-8", errors="ignore")

    if "WP_Query" not in content:
        return []
    if "homeboy-ignore-file wp.test.mock_over_fixture" in content:
        return []

    findings: list[Finding] = []
    rel = path.relative_to(root).as_posix()

    for method_match in TEST_METHOD_RE.finditer(content):
        open_brace = content.find("{", method_match.start("sig"))
        if open_brace < 0:
            continue
        extracted = extract_braced_body(content, open_brace)
        if not extracted:
            continue
        body, _ = extracted
        for query_match in NEW_QUERY_RE.finditer(body):
            if has_ignore(body, query_match.start()):
                continue
            var_name = query_match.group("var")
            fields = assigned_query_fields(body, var_name)
            if not fields:
                continue
            findings.append(
                Finding(
                    file=rel,
                    line=line_number(content, open_brace + 1 + query_match.start()),
                    method=method_match.group("name"),
                    variable=var_name,
                    fields=fields,
                )
            )

    return findings


def iter_test_files(root: Path) -> list[Path]:
    tests_dir = root / "tests"
    if not tests_dir.is_dir():
        return []
    return sorted(p for p in tests_dir.rglob("*.php") if p.is_file())


def message_for(finding: Finding) -> str:
    fields = ", ".join(f"->{field}" for field in finding.fields)
    return (
        f"{SMELL_CODE}: {finding.method}() builds ${finding.variable} = new WP_Query() "
        f"and manually assigns {fields}. Prefer real fixtures: create posts with "
        "self::factory()->post->create(), then call query_posts([...]) or instantiate "
        "WP_Query with query args so WordPress populates posts/post_count/found_posts."
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Detect WordPress test mock-over-fixture smells.")
    parser.add_argument("component_path", nargs="?", default=".")
    parser.add_argument("--json", action="store_true", dest="json_output")
    args = parser.parse_args()

    root = Path(args.component_path).resolve()
    findings: list[Finding] = []
    for path in iter_test_files(root):
        findings.extend(scan_file(path, root))

    if args.json_output:
        print(json.dumps({"findings": [f.as_dict() for f in findings]}, indent=2))
    elif findings:
        print("")
        print("============================================")
        print("ERROR: WordPress test smell detected")
        print("============================================")
        for finding in findings:
            print(f"{finding.file}:{finding.line}: {message_for(finding)}")
        print("")
        print("Fix: use WordPress test factories and query APIs instead of manual WP_Query state.")
        print("Suppress only intentional cases with: // homeboy-ignore wp.test.mock_over_fixture")
        print("")

    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
