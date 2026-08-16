#!/usr/bin/env python3
"""Shared reader for nextest ``libtest-json-plus`` event streams.

``test-runner.sh`` counts nextest output in two places: shard replay, which
reconciles what it read against a planned shard manifest, and unsharded runs,
which have no manifest to reconcile against. The reading half is identical in
both and lives here; only the reconciliation half is the shard's own.

Splitting it this way is the point. A second parser written for the unsharded
path would be free to drift from the shard path on what nextest even said --
how identity is encoded, which events are outcomes, which lines are not JSON at
all -- and the two counters would disagree about the same stream while both
looked correct in isolation.
"""

from __future__ import annotations

import json

# Terminal libtest outcomes. nextest emits lifecycle events ("queued",
# "started", "running") on the same stream and those are not outcomes, so every
# consumer has to filter against this set rather than against "has an event".
PASSED_STATUSES = frozenset({"ok", "passed"})
FAILED_STATUSES = frozenset({"failed", "fail"})
SKIPPED_STATUSES = frozenset({"ignored", "skipped"})
TERMINAL_STATUSES = PASSED_STATUSES | FAILED_STATUSES | SKIPPED_STATUSES

# nextest's retry count is u32 and its attempt number is retry_count + 1.
# Its TestInstanceId appends the decimal attempt only when it exceeds one.
MAX_RETRY_ATTEMPT = 2**32


class MalformedIdentity(Exception):
    """An emitted event name is not a nextest test identity."""


def emitted_identity(value):
    """Split an emitted event name into ``(package, target, test)``.

    libtest-json-plus omits Cargo's target kind and encodes its remaining
    identity as ``package::target$test``. Callers reconcile that structured
    projection rather than mutating either serialized identifier.
    """
    if not isinstance(value, str):
        raise MalformedIdentity("nextest emitted a malformed test identity")
    package_target, separator, test = value.rpartition("$")
    package, target_separator, target = package_target.partition("::")
    if not separator or not target_separator or not all((package, target, test)):
        raise MalformedIdentity(f"nextest emitted a malformed test identity: {value!r}")
    return package, target, test


def retry_base_identity(emitted):
    """Return the pre-retry identity for ``emitted``.

    ``None`` means the name carries no attempt suffix at all. The string
    ``"invalid"`` means it carries something shaped like one that nextest would
    never have written, which is a different finding from carrying none and is
    reported differently by the shard path.
    """
    package, target, test = emitted
    base, marker, attempt_text = test.rpartition("#")
    if not marker:
        return None
    if not base or not attempt_text.isdecimal():
        return "invalid"
    attempt = int(attempt_text)
    if attempt < 2 or attempt > MAX_RETRY_ATTEMPT or attempt_text != str(attempt):
        return "invalid"
    return package, target, base


def read_test_events(path):
    """Yield ``(name, identity, status)`` for every ``type: test`` event.

    ``identity`` is ``None`` when the emitted name is not a nextest identity.
    Child processes inherit libtest JSON, so a captured stream legitimately
    carries names no scheduler ever planned; deciding what to do with those is
    the caller's, since the shard path answers it from its manifest and the
    unsharded path has no manifest to answer it from.

    Non-JSON lines are skipped rather than treated as corruption: the runner
    captures stdout and stderr into one file, so nextest's own human-readable
    progress output shares this stream with the JSON.
    """
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict):
                continue
            if event.get("type") != "test":
                continue
            name = event.get("name")
            try:
                identity = emitted_identity(name)
            except MalformedIdentity:
                identity = None
            yield name, identity, event.get("event")
