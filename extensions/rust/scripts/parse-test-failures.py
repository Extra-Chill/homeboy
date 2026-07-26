#!/usr/bin/env python3
"""Distill Cargo test failures without making Homeboy parse Cargo output."""

import hashlib
import json
import os
import re
import sys

MAX_FAILURES = 100
MAX_FIELD_BYTES = 1024
MAX_EXCERPT_BYTES = 4000
def bounded(value, limit):
    return value.encode("utf-8")[:limit].decode("utf-8", "ignore").strip()


def relpath(value, project):
    if not value:
        return None
    try:
        relative = os.path.relpath(value, project)
    except ValueError:
        return value
    return relative if not relative.startswith("../") else value


def failure(test_id, output, project, location=None, message=None,
            failure_type="test_failure"):
    identity = bounded(test_id, MAX_FIELD_BYTES) or "cargo test"
    location = bounded(relpath(location, project), MAX_FIELD_BYTES) if location else None
    message = bounded(message or "Rust test failed; inspect the complete Cargo output.", MAX_FIELD_BYTES)
    fingerprint_namespace = "rust:cargo-test:failed" if identity == "cargo test" else "rust:test:" + identity
    fingerprint = hashlib.sha256(fingerprint_namespace.encode()).hexdigest()
    return {
        # v1 fields remain authoritative for existing sidecar consumers.
        "test_id": identity,
        "suite": None,
        "file": location.rsplit(":", 2)[0] if location else None,
        "line": int(location.rsplit(":", 2)[1]) if location and re.search(r":\d+:\d+$", location) else None,
        "message": message,
        "failure_type": failure_type,
        "fingerprint": fingerprint,
        "stdout_excerpt": bounded(output[-MAX_EXCERPT_BYTES:], MAX_EXCERPT_BYTES),
        "stderr_excerpt": "",
    }


def add(records, seen, test_id, output, project, location=None, message=None):
    if not test_id:
        return
    if test_id in seen:
        if location or message:
            for index, record in enumerate(records):
                if record["test_id"] == test_id:
                    records[index] = failure(test_id, output, project, location, message)
                    break
        return
    if len(records) >= MAX_FAILURES:
        return
    seen.add(test_id)
    records.append(failure(test_id, output, project, location, message))


def panic_message(lines, start):
    for raw in lines[start:]:
        candidate = raw.strip()
        if not candidate:
            return None
        if candidate.startswith("note:"):
            continue
        if candidate.startswith(("---- ", "test ", "failures:", "error:")):
            return None
        return candidate
    return None


def parse(output, project):
    records = []
    seen = set()
    lines = output.splitlines()
    current = None

    for index, raw in enumerate(lines):
        section = re.match(r"^---- (?P<name>.+) stdout ----$", raw)
        if section:
            current = section.group("name")
            continue
        if raw.startswith("---- ") and raw.endswith(" ----"):
            current = None
            continue

        failed = re.match(r"^test (?P<name>.+) \.\.\. FAILED$", raw)
        if failed:
            add(records, seen, failed.group("name"), output, project)

        panic = re.match(r"^thread '(?P<name>.+)' panicked at (?P<location>.+):$", raw)
        if panic:
            # A libtest section names the failed test even when the panic came
            # from a worker thread instead of the test thread itself.
            add(records, seen, current or panic.group("name"), output, project,
                panic.group("location"), panic_message(lines, index + 1))

    failures_header = next((index for index, line in enumerate(lines) if line.strip() == "failures:"), None)
    if failures_header is not None:
        for raw in lines[failures_header + 1:]:
            if raw.startswith("test result:") or raw.startswith(("error:", "failures:")):
                break
            if not raw.startswith((" ", "\t")):
                break
            candidate = raw.strip()
            if candidate:
                add(records, seen, candidate, output, project)

    return records


if len(sys.argv) != 4:
    raise SystemExit("usage: parse-test-failures.py PROJECT OUTPUT_FILE TARGET")

PROJECT, OUTPUT_FILE, TARGET = sys.argv[1:]
with open(OUTPUT_FILE, "rb") as handle:
    OUTPUT_BYTES = handle.read()
OUTPUT = OUTPUT_BYTES.decode("utf-8", errors="replace")

failures = parse(OUTPUT, PROJECT)
if not failures:
    fallback = failure(
        "cargo test", OUTPUT, PROJECT,
        message="cargo test failed before individual test failures could be parsed",
        failure_type="infrastructure",
    )
    failures = [fallback]

with open(TARGET, "w", encoding="utf-8") as handle:
    json.dump(failures, handle, indent=2)
    handle.write("\n")
