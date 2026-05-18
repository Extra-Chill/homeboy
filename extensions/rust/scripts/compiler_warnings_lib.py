#!/usr/bin/env python3
"""Shared Cargo JSON parsing for Homeboy compiler warning contracts."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any


def run_cargo_check(root: Path) -> str:
    if not (root / "Cargo.toml").exists():
        return ""

    completed = subprocess.run(
        ["cargo", "check", "--message-format=json"],
        cwd=root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    return completed.stdout


def relative_path(root: Path, file_name: str) -> str:
    if not file_name:
        return ""

    try:
        return str(Path(file_name).resolve().relative_to(root.resolve()))
    except ValueError:
        root_prefix = str(root)
        if file_name.startswith(root_prefix):
            return file_name[len(root_prefix) :].lstrip("/")
        return file_name


def warning_messages(stdout: str) -> list[dict[str, Any]]:
    messages: list[dict[str, Any]] = []
    for line in stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-message":
            continue
        compiler_message = message.get("message") or {}
        if compiler_message.get("level") != "warning":
            continue
        messages.append(compiler_message)
    return messages


def warning_code(message: dict[str, Any]) -> str:
    code = message.get("code") or {}
    return str(code.get("code") or "unknown")


def primary_span(message: dict[str, Any]) -> dict[str, Any] | None:
    spans = message.get("spans") or []
    for span in spans:
        if span.get("is_primary") is True:
            return span
    return spans[0] if spans else None


def warning_suggestion(code: str) -> str:
    suggestions = {
        "dead_code": "Remove the unused item or add `#[allow(dead_code)]` if intentionally reserved",
        "unused_imports": "Remove the unused import",
        "unused_variables": "Prefix with underscore `_` or remove the variable",
        "unused_assignments": "Remove the unnecessary assignment",
        "unused_mut": "Remove the `mut` qualifier",
        "unreachable_code": "Remove or refactor the unreachable code path",
        "unused_must_use": "Handle the return value or explicitly ignore with `let _ = ...`",
    }
    return suggestions.get(code, f"Address compiler warning: {code}")


def span_original_text(span: dict[str, Any]) -> str:
    text = span.get("text") or []
    if not text:
        return ""
    return str((text[0] or {}).get("text") or "")


def span_replaced_text(span: dict[str, Any]) -> str:
    original_text = span_original_text(span)
    if not original_text:
        return ""

    line_start = int(span.get("line_start") or 1)
    line_end = int(span.get("line_end") or line_start)
    if line_start != line_end:
        return original_text

    column_start = int(span.get("column_start") or 1)
    column_end = int(span.get("column_end") or column_start)
    start = max(column_start - 1, 0)
    end = max(column_end - 1, start)
    if start < len(original_text) and end <= len(original_text):
        return original_text[start:end]
    return original_text
