#!/usr/bin/env python3
"""Rust refactor script — command dispatch entry point.

Receives JSON commands on stdin, outputs JSON results on stdout.
"""

import json
import sys
from dataclasses import asdict

from .parsing import parse_items
from .tests import find_related_tests
from .imports import resolve_imports
from .visibility import adjust_visibility
from .rewrite import rewrite_caller_imports
from .struct_fields import propagate_struct_fields
from .module_index import generate_module_index


def handle_parse_items(data: dict) -> dict:
    items = parse_items(data.get("content", ""))
    return {"items": [asdict(item) for item in items]}


def handle_resolve_imports(data: dict) -> dict:
    return resolve_imports(
        data.get("moved_items", []),
        data.get("source_content", ""),
        data.get("source_path", ""),
        data.get("dest_path", ""),
    )


def handle_find_related_tests(data: dict) -> dict:
    return find_related_tests(
        data.get("item_names", []),
        data.get("content", ""),
    )


def handle_adjust_visibility(data: dict) -> dict:
    return adjust_visibility(
        data.get("items", []),
        data.get("source_path", ""),
        data.get("dest_path", ""),
    )


def handle_rewrite_caller_imports(data: dict) -> dict:
    return rewrite_caller_imports(
        data.get("item_names", []),
        data.get("source_module_path", ""),
        data.get("dest_module_path", ""),
        data.get("file_content", ""),
        data.get("file_path", ""),
    )


def handle_propagate_struct_fields(data: dict) -> dict:
    return propagate_struct_fields(data)


def handle_generate_module_index(data: dict) -> dict:
    return generate_module_index(
        data.get("submodules", []),
        data.get("remaining_content", ""),
    )


COMMANDS = {
    "parse_items": handle_parse_items,
    "resolve_imports": handle_resolve_imports,
    "find_related_tests": handle_find_related_tests,
    "adjust_visibility": handle_adjust_visibility,
    "rewrite_caller_imports": handle_rewrite_caller_imports,
    "propagate_struct_fields": handle_propagate_struct_fields,
    "generate_module_index": handle_generate_module_index,
}


def main():
    raw = sys.stdin.read()
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        print(json.dumps({"error": f"Invalid JSON: {e}"}), file=sys.stderr)
        sys.exit(1)

    command = data.get("command", "")
    handler = COMMANDS.get(command)
    if handler is None:
        print(json.dumps({"error": f"Unknown command: {command}"}), file=sys.stderr)
        sys.exit(1)

    try:
        result = handler(data)
        print(json.dumps(result))
    except Exception as e:
        print(json.dumps({"error": str(e)}), file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
