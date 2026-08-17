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


def settings():
    raw_settings = os.environ.get("HOMEBOY_SETTINGS_JSON") or "{}"
    try:
        value = json.loads(raw_settings)
    except json.JSONDecodeError:
        value = {}
    return value if isinstance(value, dict) else {}


def host_test_path(project, plugin_slug, sandbox_path):
    mounts = settings().get("wp_codebox_phpunit_mounts") or []
    for mount in sorted(mounts, key=lambda item: len(item.get("target", "")) if isinstance(item, dict) else 0, reverse=True):
        if not isinstance(mount, dict):
            continue
        source = mount.get("source")
        target = mount.get("target")
        if not isinstance(source, str) or not isinstance(target, str):
            continue
        target = target.rstrip("/")
        if sandbox_path != target and not sandbox_path.startswith(f"{target}/"):
            continue
        return Path(source).joinpath(sandbox_path[len(target):].lstrip("/"))

    plugin_root = f"/wordpress/wp-content/plugins/{plugin_slug}/"
    if sandbox_path.startswith(plugin_root):
        return project / sandbox_path[len(plugin_root):]

    fail(f"discovered PHPUnit file has no declared host mount: {sandbox_path}")


def enumerate_tests(project, package, discovery_file):
    try:
        discovery = json.loads(Path(discovery_file).read_text())
    except OSError as error:
        fail(f"could not read WP Codebox discovery result: {error}")
    except json.JSONDecodeError as error:
        fail(f"WP Codebox discovery result is not valid JSON: {error}")
    if not isinstance(discovery, dict) or discovery.get("schema") != "wp-codebox/phpunit-discovery/v1":
        fail("WP Codebox discovery result has an unsupported schema")
    plugin_slug = discovery.get("plugin_slug")
    files = discovery.get("files")
    if not isinstance(plugin_slug, str) or not plugin_slug or not isinstance(files, list) or not files:
        fail("WP Codebox discovery result has no plugin identity or files")
    if any(not isinstance(path, str) or not path.startswith("/") for path in files):
        fail("WP Codebox discovery files must be unique sandbox-absolute paths")
    if len(files) != len(set(files)):
        fail("WP Codebox discovery files must be unique sandbox-absolute paths")

    tests = {}
    project = project.resolve()
    for sandbox_path in files:
        path = host_test_path(project, plugin_slug, sandbox_path).resolve()
        try:
            relative = str(path.relative_to(project))
        except ValueError:
            fail(f"configured PHPUnit test file escapes the component: {path}")
        if not path.is_file():
            fail(f"discovered PHPUnit test file is missing on the host: {path}")
        tests[relative] = {
            "id": relative,
            "package": package,
            "target": "phpunit",
            "target_kind": "test",
            "name": path.name,
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
    parser.add_argument("--discovery-file", required=True, help="WP Codebox canonical discovery result")
    parser.add_argument("--output", required=True, help="file to write the inventory to")
    args = parser.parse_args()

    config = load_inventory_config(args.extension_path)
    root = workspace_root(args.project, config)
    package = args.package or Path(args.project).resolve().name

    tests = enumerate_tests(Path(args.project).resolve(), package, args.discovery_file)
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
