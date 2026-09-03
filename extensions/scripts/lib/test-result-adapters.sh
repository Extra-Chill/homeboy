#!/usr/bin/env bash
# Shared test-result parser adapters for extension runner wrappers.

homeboy_parse_test_results_with_adapters() {
    local output_file="$1"
    shift || true

    if [ -z "${output_file:-}" ] || [ ! -f "$output_file" ]; then
        return 0
    fi

    if ! type homeboy_write_test_results >/dev/null 2>&1; then
        return 0
    fi

    local parsed
    parsed=$(python3 - "$output_file" "$@" <<'PY'
import re
import sys

output_file = sys.argv[1]
adapters = sys.argv[2:]

try:
    with open(output_file, encoding="utf-8") as handle:
        text = handle.read()
except OSError:
    sys.exit(0)


def emit(total, passed, failed, skipped, partial=""):
    print("\t".join(str(value) for value in (total, passed, failed, skipped, partial)))
    sys.exit(0)


def count_after(label, line):
    match = re.search(rf"{re.escape(label)}\s*(\d+)", line)
    return int(match.group(1)) if match else 0


def count_before(label, line):
    match = re.search(rf"(\d+)\s+{re.escape(label)}", line)
    return int(match.group(1)) if match else 0


def parse_phpunit():
    ok_match = re.search(r"OK \((\d+) tests?", text)
    if ok_match:
        total = int(ok_match.group(1))
        emit(total, total, 0, 0)
        return True

    summary_lines = [line for line in text.splitlines() if re.match(r"^Tests: \d+", line)]
    if not summary_lines:
        return False
    summary = summary_lines[-1]
    total = count_after("Tests:", summary)
    failed = count_after("Errors:", summary) + count_after("Failures:", summary)
    skipped = (
        count_after("Skipped:", summary)
        + count_after("Incomplete:", summary)
        + count_after("Risky:", summary)
        + count_after("Warnings:", summary)
    )
    passed = max(total - failed - skipped, 0)
    emit(total, passed, failed, skipped)
    return True


def parse_phpunit_testdox():
    passed = len(re.findall(r"^ ✔", text, flags=re.MULTILINE))
    failed = len(re.findall(r"^ ✘", text, flags=re.MULTILINE))
    if passed == 0 and failed == 0:
        return False
    emit(passed + failed, passed, failed, 0, "testdox-fallback")
    return True


def parse_host_smoke():
    summary_match = re.search(r"^HOST_SMOKE_SUMMARY:passed=(\d+) failed=(\d+)\b", text, flags=re.MULTILINE)
    if summary_match:
        passed = int(summary_match.group(1))
        failed = int(summary_match.group(2))
        emit(passed + failed, passed, failed, 0)
        return True

    failed = len(re.findall(r"^HOST_SMOKE_FAIL:.+:exit=\d+", text, flags=re.MULTILINE))
    if failed == 0:
        return False

    passed = len(re.findall(r"^HOST_SMOKE_OK:", text, flags=re.MULTILINE))
    emit(passed + failed, passed, failed, 0, "host-smoke-failure")
    return True


def parse_cargo_test():
    passed = failed = skipped = 0
    matched = False
    for line in text.splitlines():
        if not line.startswith("test result:"):
            continue
        matched = True
        passed += count_before("passed", line)
        failed += count_before("failed", line)
        skipped += count_before("ignored", line)
    if not matched:
        return False
    emit(passed + failed + skipped, passed, failed, skipped)
    return True


def parse_node_test():
    # A WordPress package script can invoke several nested Node TAP suites. Its
    # terminal package summary is the authoritative invocation count.
    summaries = re.findall(
        r"^passed:\s*(\d+);\s*failed:\s*(\d+);\s*intentionally skipped:\s*(\d+)\s*$",
        text,
        flags=re.MULTILINE,
    )
    if summaries:
        passed, failed, skipped = map(int, summaries[-1])
        emit(passed + failed + skipped, passed, failed, skipped)
        return True

    # Without a package summary, use one complete Node TAP summary rather than
    # adding nested summaries from separately invoked test commands.
    tap_summaries = re.findall(
        r"^# tests\s+(\d+)\s*$.*?^# pass\s+(\d+)\s*$.*?^# fail\s+(\d+)\s*$.*?^# skipped\s+(\d+)\s*$",
        text,
        flags=re.MULTILINE | re.DOTALL,
    )
    if not tap_summaries:
        return False
    total, passed, failed, skipped = map(int, tap_summaries[-1])
    emit(total, passed, failed, skipped)
    return True


def parse_jest():
    # Jest and wp-scripts write their terminal summary to stderr, so callers
    # must capture both streams. The per-category counts appear on one line as
    # "Tests: 1 failed, 2 skipped, 5 passed, 8 total"; categories are omitted
    # when zero, so each is read independently rather than positionally.
    summaries = [
        line
        for line in text.splitlines()
        if re.match(r"^Tests:\s+.*\b(\d+)\s+total\s*$", line.strip())
    ]
    if not summaries:
        return False
    summary = summaries[-1].strip()
    total = count_before("total", summary)
    passed = count_before("passed", summary)
    failed = count_before("failed", summary)
    # Jest reports "skipped" for it.skip and "todo" for it.todo. Both are
    # unexecuted, so they fold together into the skipped count.
    skipped = count_before("skipped", summary) + count_before("todo", summary)
    # A crash after the summary can leave the counts internally inconsistent.
    # Trust total and fold the unexplained remainder into failed rather than
    # reporting a run as fully passing.
    if total > passed + failed + skipped:
        failed += total - passed - failed - skipped
    emit(total, passed, failed, skipped)
    return True


adapter_functions = {
    "host-smoke": parse_host_smoke,
    "phpunit": parse_phpunit,
    "phpunit-testdox": parse_phpunit_testdox,
    "cargo-test": parse_cargo_test,
    "node-test": parse_node_test,
    "jest": parse_jest,
}

for adapter in adapters:
    parser = adapter_functions.get(adapter)
    if parser and parser():
        break
PY
    )

    [ -n "$parsed" ] || return 0

    local total passed failed skipped partial
    IFS=$'\t' read -r total passed failed skipped partial <<EOF
$parsed
EOF

    homeboy_write_test_results "${total:-0}" "${passed:-0}" "${failed:-0}" "${skipped:-0}" "${partial:-}"
}
