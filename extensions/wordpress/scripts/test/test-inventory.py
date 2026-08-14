#!/usr/bin/env python3
"""Emit a `homeboy/test-inventory/v1` document for a WordPress component.

Sharded test execution needs an enumeration of the suite that is produced
*without running it*, bound to a fingerprint of the workspace and of the runner.
Homeboy core owns the binding and re-derives both fingerprints itself before
accepting anything this script writes, so the only way a document is accepted is
by computing them exactly the way core does.

Two details are load-bearing and easy to get wrong:

* The canonical JSON that feeds `inventory_fingerprint` is
  `json.dumps(record, sort_keys=True, separators=(",", ":"))` with the default
  `ensure_ascii=True`, over the record *without* `inventory_fingerprint`.
  Core reproduces that byte layout by hand in `canonical_inventory_json`,
  including the ASCII escaping, so a differently-encoded equivalent document is
  rejected. This is also why the producer is Python rather than PHP or shell:
  matching Python's `json.dumps` is the contract.

* The workspace fingerprint concatenates `f"{relative_path}\\0{text}\\0"` over
  the selected files in sorted order, where `text` has been read with universal
  newlines. Core normalizes CRLF and lone CR to LF for the same reason.

The file selection, skip list, root markers and runner identity are read from
the extension manifest's `test.inventory` block, so this script and the
declaration Homeboy validates against cannot drift apart.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

SCHEMA = "homeboy/test-inventory/v1"


def fail(message):
    raise SystemExit(f"WordPress test inventory error: {message}")


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def load_inventory_config(extension_path):
    manifest = Path(extension_path) / "wordpress.json"
    try:
        declared = json.loads(manifest.read_text())
    except OSError as error:
        fail(f"could not read {manifest}: {error}")
    except json.JSONDecodeError as error:
        fail(f"{manifest} is not valid JSON: {error}")

    config = declared.get("test", {}).get("inventory")
    if not config:
        fail("wordpress.json declares no test.inventory block")
    return config


def workspace_root(project, config):
    """Mirror core's `inventory_workspace_root`.

    Walk upward for the first ancestor holding a declared marker; with none
    found the component path is its own root. Core resolves this identically,
    and a mismatch means the workspace fingerprints cannot agree.
    """
    start = Path(project).resolve()
    markers = config.get("root_markers") or []
    for candidate in [start, *start.parents]:
        if any((candidate / marker).exists() for marker in markers):
            return candidate
    return start


def selected_for_fingerprint(path, config):
    names = set(config.get("fingerprint_names") or [])
    extensions = set(config.get("fingerprint_extensions") or [])
    if path.name in names:
        return True
    return path.suffix[1:] in extensions if path.suffix else False


def workspace_fingerprint(root, config):
    skip = set(config.get("fingerprint_skip_dirs") or [])
    selected = []
    for directory, subdirectories, files in os.walk(root):
        subdirectories[:] = [name for name in subdirectories if name not in skip]
        for name in files:
            path = Path(directory) / name
            if path.is_file() and selected_for_fingerprint(path, config):
                selected.append(path)

    content = []
    for path in sorted(selected, key=lambda item: str(item.relative_to(root))):
        try:
            # `read_text` applies universal newlines, which is what core's
            # CRLF/CR normalization reproduces.
            text = path.read_text(errors="replace")
        except OSError as error:
            fail(f"could not read {path} for the workspace fingerprint: {error}")
        content.append(f"{path.relative_to(root)}\0{text}\0")
    return digest("".join(content))


def runner_fingerprint(root, config, runner):
    declared = next(
        (entry for entry in config.get("runners") or [] if entry.get("id") == runner),
        None,
    )
    if not declared:
        fail(f"wordpress.json declares no inventory runner named {runner!r}")
    argv = declared.get("version_command") or []
    if not argv:
        fail(f"inventory runner {runner!r} declares no version_command")
    try:
        result = subprocess.run(
            argv, cwd=root, text=True, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, check=False,
        )
    except OSError as error:
        fail(f"could not fingerprint {runner} runner: {error}")
    if result.returncode:
        fail(f"could not fingerprint {runner} runner: {result.stderr.strip()}")
    return digest(f"{runner}\0{result.stdout.strip()}")


def within_tests_tree(relative):
    """Match the runner's own `tests/` and `wordpress/tests/` prefixes."""
    parts = relative.split("/")
    if parts[0] == "tests":
        return True
    return len(parts) > 1 and parts[0] == "wordpress" and parts[1] == "tests"


def classify(relative):
    """Return (target_kind, target) or None, mirroring the runner's matchers.

    These predicates are the ones `test-runner.sh` uses to route a changed-scope
    selection, so the inventory enumerates exactly the population the runner
    would execute. A file the runner cannot route must not appear here: a shard
    would then claim a test that never runs, and the aggregate totals would not
    reconcile.
    """
    name = relative.rsplit("/", 1)[-1]
    in_tests = within_tests_tree(relative)

    if in_tests and name.endswith("-smoke.php"):
        return "smoke", "host-php-smoke"
    if in_tests and name.endswith("-smoke.js"):
        return "smoke", "host-js-smoke"
    if in_tests and name.endswith("-smoke.sh"):
        return "smoke", "host-shell-smoke"
    for suffix in (".test.js", ".test.cjs", ".test.mjs", ".test.jsx", ".test.ts", ".test.tsx"):
        if name.endswith(suffix):
            return "test", "node-test"
    if name.endswith("Test.php") or name.startswith("test-") and name.endswith(".php"):
        return "test", "phpunit"
    return None


def enumerate_tests(root, package, config):
    skip = set(config.get("fingerprint_skip_dirs") or [])
    tests = {}
    for directory, subdirectories, files in os.walk(root):
        subdirectories[:] = [name for name in subdirectories if name not in skip]
        for name in files:
            relative = str((Path(directory) / name).relative_to(root))
            classified = classify(relative)
            if not classified:
                continue
            target_kind, target = classified
            # The path is the id: it is what HOMEBOY_CHANGED_TEST_FILES already
            # carries, so a shard manifest slice can be replayed through the
            # existing changed-scope routing without a second selector format.
            tests[relative] = {
                "id": relative,
                "package": package,
                "target": target,
                "target_kind": target_kind,
                "name": name,
                "expected_outcome": "executed",
            }
    return [tests[key] for key in sorted(tests)]


def build_inventory(runner, runner_digest, workspace_digest, tests):
    record = {
        "schema": SCHEMA,
        "runner": runner,
        "runner_fingerprint": runner_digest,
        "workspace_fingerprint": workspace_digest,
        "tests": tests,
    }
    # Core hashes the record without `inventory_fingerprint`, and deliberately
    # excludes `fallback_reason` from the canonical form, so this producer never
    # emits one.
    record["inventory_fingerprint"] = digest(
        json.dumps(record, sort_keys=True, separators=(",", ":"))
    )
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", required=True, help="component source path")
    parser.add_argument("--extension-path", required=True, help="extension root holding wordpress.json")
    parser.add_argument("--runner", default="wordpress", help="declared runner identity")
    parser.add_argument("--package", default="", help="package label recorded on each test")
    parser.add_argument("--output", required=True, help="file to write the inventory to")
    args = parser.parse_args()

    config = load_inventory_config(args.extension_path)
    root = workspace_root(args.project, config)
    package = args.package or Path(args.project).resolve().name

    tests = enumerate_tests(root, package, config)
    if not tests:
        # Core refuses an empty inventory, and it is right to: an empty
        # enumeration cannot be distinguished from a broken producer, and
        # sharding nothing would report a green suite that ran no tests.
        fail(f"no test files found under {root}")

    inventory = build_inventory(
        args.runner,
        runner_fingerprint(root, config, args.runner),
        workspace_fingerprint(root, config),
        tests,
    )

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(inventory, sort_keys=True, separators=(",", ":")))
    print(f"WordPress test inventory: {len(tests)} tests -> {output}", file=sys.stderr)


if __name__ == "__main__":
    main()
