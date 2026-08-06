#!/usr/bin/env python3
"""Build and validate Rust test shard inventories without adding core contracts."""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path

SCHEMA = "homeboy/test-inventory/v1"
MANIFEST_SCHEMA = "homeboy/test-shard-manifest/v1"


def fail(message):
    raise SystemExit(f"Rust test shard error: {message}")


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def run(command, cwd):
    return subprocess.run(command, cwd=cwd, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, check=False)


def workspace_fingerprint(root):
    root = Path(root).resolve()
    files = sorted(
        path for path in root.rglob("*")
        if path.is_file()
        and ".git" not in path.parts
        and "target" not in path.parts
        and (path.name in {"Cargo.toml", "Cargo.lock"} or path.suffix == ".rs")
    )
    content = "".join(f"{path.relative_to(root)}\0{path.read_text()}\0" for path in files)
    return digest(content)


def runner_fingerprint(cwd, runner):
    result = run(["cargo", "nextest", "--version"] if runner == "nextest" else ["cargo", "--version"], cwd)
    if result.returncode:
        fail(f"could not fingerprint {runner} runner: {result.stderr.strip()}")
    return digest(f"{runner}\0{result.stdout.strip()}")


def cargo_inventory(workspace_root, packages):
    tests = []
    command = ["cargo", "test", "--workspace", "--no-run", "--message-format=json"]
    result = run(command, workspace_root)
    if result.returncode:
        fail(f"could not enumerate test executables: {result.stderr.strip()}")
    for line in result.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (message.get("reason") != "compiler-artifact"
                or not message.get("executable")
                or not message.get("profile", {}).get("test")):
            continue
        target = message.get("target", {})
        kinds = target.get("kind", [])
        listed = run([message["executable"], "--list"], workspace_root)
        if listed.returncode:
            fail(f"could not list tests for {target.get('name', 'unknown target')}: {listed.stderr.strip()}")
        package = packages.get(message.get("package_id"))
        if not package:
            fail("cargo emitted a test executable without a resolvable package")
        target_kind = kinds[0] if kinds else "unknown"
        for test_line in listed.stdout.splitlines():
            name, separator, _kind = test_line.partition(": ")
            if separator and name:
                tests.append({"id": f"{package}::{target_kind}::{target['name']}::{name}", "package": package, "target": target["name"], "target_kind": target_kind, "name": name})
    return tests


def nextest_inventory(workspace_root):
    result = run(["cargo", "nextest", "list", "--workspace", "--run-ignored", "all", "--message-format", "json"], workspace_root)
    if result.returncode:
        fail(f"could not enumerate nextest tests: {result.stderr.strip()}")
    try:
        listed = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"nextest emitted invalid test list JSON: {error}")
    tests = []
    for suite in listed.get("rust-suites", {}).values():
        package = suite.get("package-name")
        target = suite.get("binary-name")
        target_kind = suite.get("kind")
        if not all(isinstance(value, str) and value for value in (package, target, target_kind)):
            fail("nextest emitted a suite without package, binary, or kind identity")
        for name, testcase in suite.get("testcases", {}).items():
            if not isinstance(name, str) or not isinstance(testcase, dict):
                fail("nextest emitted an invalid testcase identity")
            if testcase.get("filter-match", {}).get("status") != "matches":
                continue
            tests.append({
                "id": f"{package}::{target_kind}::{target}::{name}",
                "package": package,
                "target": target,
                "target_kind": target_kind,
                "name": name,
                # nextest list is the canonical source for tests that its
                # default run policy intentionally skips without a terminal.
                "expected_outcome": "skipped" if testcase.get("ignored") else "executed",
            })
    return tests


def inventory(project, runner):
    metadata_result = run(["cargo", "metadata", "--no-deps", "--format-version=1"], project)
    if metadata_result.returncode:
        fail(f"cargo metadata failed: {metadata_result.stderr.strip()}")
    metadata = json.loads(metadata_result.stdout)
    workspace_root = Path(metadata["workspace_root"]).resolve()
    packages = {package["id"]: package["name"] for package in metadata["packages"]}
    tests = nextest_inventory(workspace_root) if runner == "nextest" else cargo_inventory(workspace_root, packages)
    # cargo-nextest does not execute doctests, so its inventory contains only
    # targets that its exact replay command can run.
    if runner == "cargo":
        # Doctests do not produce compiler-artifact executables. Cargo's stable
        # list output still identifies the package in its Doc-tests heading.
        docs = run(["cargo", "test", "--workspace", "--doc", "--", "--list"], workspace_root)
        if docs.returncode:
            fail(f"could not list doctests: {docs.stderr.strip()}")
        doc_package = None
        package_names = set(packages.values())
        for line in docs.stdout.splitlines():
            stripped = line.strip()
            if stripped.startswith("Doc-tests "):
                candidate = stripped.removeprefix("Doc-tests ").strip()
                doc_package = candidate if candidate in package_names else None
                continue
            name, separator, _kind = stripped.partition(": ")
            if not separator or not name or not doc_package:
                continue
            tests.append({
                "id": f"{doc_package}::doc::doc::{name}",
                "package": doc_package,
                "target": "doc",
                "target_kind": "doc",
                "name": name,
            })
    tests.sort(key=lambda item: item["id"])
    if len({item["id"] for item in tests}) != len(tests):
        fail("cargo produced duplicate fully qualified test identities")
    record = {
        "schema": SCHEMA,
        "runner": runner,
        "runner_fingerprint": runner_fingerprint(workspace_root, runner),
        "workspace_fingerprint": workspace_fingerprint(workspace_root),
        "tests": tests,
    }
    record["inventory_fingerprint"] = digest(json.dumps(record, sort_keys=True, separators=(",", ":")))
    return record


def legacy_inventory_fingerprint(current):
    """Fingerprint the v1 nextest inventory emitted before ignored tests were listed."""
    legacy_tests = []
    for test in current["tests"]:
        if test.get("expected_outcome") == "skipped":
            continue
        legacy_tests.append({key: test[key] for key in ("id", "package", "target", "target_kind", "name")})
    legacy = {
        "schema": SCHEMA,
        "runner": current["runner"],
        "runner_fingerprint": current["runner_fingerprint"],
        "workspace_fingerprint": current["workspace_fingerprint"],
        "tests": legacy_tests,
    }
    return digest(json.dumps(legacy, sort_keys=True, separators=(",", ":"))), {test["id"] for test in legacy_tests}


def validate(manifest_path, current):
    try:
        manifest = json.loads(Path(manifest_path).read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"invalid shard manifest: {error}")
    if manifest.get("schema") != MANIFEST_SCHEMA:
        fail("unsupported shard manifest schema")
    if manifest.get("runner") != current.get("runner"):
        fail("shard manifest runner does not match the selected runner")
    for key in ("runner_fingerprint", "workspace_fingerprint"):
        if manifest.get(key) != current.get(key):
            fail(f"stale shard manifest: {key} does not match the current inventory")
    selected = manifest.get("tests")
    if not isinstance(selected, list) or not selected:
        fail("shard manifest must contain a non-empty tests array")
    if any(not isinstance(identity, str) for identity in selected):
        fail("shard manifest tests must be strings")
    if len(set(selected)) != len(selected):
        fail("shard manifest contains duplicate test identities")
    known = {test["id"]: test for test in current["tests"]}
    missing = [identity for identity in selected if identity not in known]
    if missing:
        fail(f"shard manifest contains unresolvable test identity: {missing[0]}")
    if manifest.get("inventory_fingerprint") != current.get("inventory_fingerprint"):
        legacy_fingerprint, legacy_ids = legacy_inventory_fingerprint(current)
        if manifest.get("inventory_fingerprint") != legacy_fingerprint:
            fail("stale shard manifest: inventory_fingerprint does not match the current or compatible legacy inventory")
        legacy_missing = [identity for identity in selected if identity not in legacy_ids]
        if legacy_missing:
            fail(f"legacy shard manifest selects tests outside the legacy inventory: {legacy_missing[0]}")
    return [known[identity] for identity in selected]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--project", required=True)
    parser.add_argument("--runner", choices=("cargo", "nextest"), required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--manifest")
    args = parser.parse_args()
    current = inventory(args.project, args.runner)
    output = current
    if args.manifest:
        output = {"inventory": current, "selected": validate(args.manifest, current)}
    Path(args.output).write_text(json.dumps(output, indent=2) + "\n")


if __name__ == "__main__":
    main()
