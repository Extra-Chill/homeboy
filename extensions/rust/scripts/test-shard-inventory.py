#!/usr/bin/env python3
"""Build and validate Rust test shard inventories without adding core contracts."""

import argparse
import hashlib
import json
import os
import subprocess
from pathlib import Path

SCHEMA = "homeboy/test-inventory/v1"
MANIFEST_SCHEMA = "homeboy/test-shard-manifest/v1"
CHANGED_SELECTION_SCHEMA = "homeboy/rust-changed-test-selection/v2"


def fail(message):
    raise SystemExit(f"Rust test shard error: {message}")


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def set_inventory_fingerprint(record):
    record = dict(record)
    record.pop("inventory_fingerprint", None)
    record["inventory_fingerprint"] = digest(json.dumps(record, sort_keys=True, separators=(",", ":")))
    return record


def project_inventory(current, tests):
    projected = {key: value for key, value in current.items() if key != "inventory_fingerprint"}
    projected["tests"] = tests
    return set_inventory_fingerprint(projected)


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


def cargo_metadata_roots(project, context):
    component_root = Path(project).resolve()
    result = run(["cargo", "metadata", "--no-deps", "--format-version=1"], component_root)
    if result.returncode:
        fail(f"cargo metadata failed {context}: {result.stderr.strip()}")
    metadata = json.loads(result.stdout)
    workspace_root = Path(metadata["workspace_root"]).resolve()
    if component_root != workspace_root and workspace_root not in component_root.parents:
        fail(f"component root is outside Cargo workspace {context}")
    return metadata, workspace_root, component_root


def cargo_inventory(workspace_root, packages):
    tests = {}
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
        ignored = run([message["executable"], "--list", "--ignored"], workspace_root)
        if listed.returncode or ignored.returncode:
            fail(f"could not list tests for {target.get('name', 'unknown target')}: {listed.stderr.strip()}")
        package = packages.get(message.get("package_id"))
        if not package:
            fail("cargo emitted a test executable without a resolvable package")
        target_kind = kinds[0] if kinds else "unknown"
        ignored_names = {
            line.partition(": ")[0]
            for line in ignored.stdout.splitlines()
            if line.partition(": ")[1] and line.partition(": ")[0]
        }
        for test_line in listed.stdout.splitlines():
            name, separator, _kind = test_line.partition(": ")
            if separator and name:
                test_id = f"{package}::{target_kind}::{target['name']}::{name}"
                tests[test_id] = {
                    "id": test_id,
                    "package": package,
                    "target": target["name"],
                    "target_kind": target_kind,
                    "name": name,
                    "expected_outcome": "skipped" if name in ignored_names else "executed",
                }
    return list(tests.values())


def nextest_archive():
    """Path to a prebuilt `cargo nextest archive`, when the caller supplies one.

    Enumerating and running tests both compile every test binary in the
    workspace. Sharded CI does that once per shard plus once for the inventory
    -- five identical full-workspace debug compiles to execute a few dozen
    tests. An archive is built once and reused, so `list` and `run` become
    execution-only.
    """
    archive = os.environ.get("HOMEBOY_RUST_NEXTEST_ARCHIVE", "").strip()
    if not archive:
        return ""
    if not os.path.isfile(archive):
        fail(f"HOMEBOY_RUST_NEXTEST_ARCHIVE does not exist: {archive}")
    return archive


def nextest_inventory(workspace_root):
    archive = nextest_archive()
    # `--archive-file` carries its own workspace, so it replaces `--workspace`
    # in the same argv position rather than joining it.
    source = ["--archive-file", archive] if archive else ["--workspace"]
    command = ["cargo", "nextest", "list", *source, "--run-ignored", "all", "--message-format", "json"]
    result = run(command, workspace_root)
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
    metadata, workspace_root, _component_root = cargo_metadata_roots(project, "while building inventory")
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
                "expected_outcome": "executed",
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
    return set_inventory_fingerprint(record)


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
    manifest_fingerprint = manifest.get("inventory_fingerprint")
    selected_ids = set(selected)
    selected_current = [test for test in current["tests"] if test["id"] in selected_ids]
    scoped_fingerprint = project_inventory(current, selected_current)["inventory_fingerprint"]
    if manifest_fingerprint not in {current.get("inventory_fingerprint"), scoped_fingerprint}:
        legacy_fingerprint, legacy_ids = legacy_inventory_fingerprint(current)
        if manifest_fingerprint != legacy_fingerprint:
            fail("stale shard manifest: inventory_fingerprint does not match the current, scoped, or compatible legacy inventory")
        legacy_missing = [identity for identity in selected if identity not in legacy_ids]
        if legacy_missing:
            fail(f"legacy shard manifest selects tests outside the legacy inventory: {legacy_missing[0]}")
    return [known[identity] for identity in selected]


def matching_tests(current, package, target_kind, target, module):
    return [
        test for test in current["tests"]
        if test["package"] == package
        and test["target_kind"] == target_kind
        and test["target"] == target
        and (module is None or test["name"] == module or test["name"].startswith(f"{module}::"))
    ]


def resolve_changed_selection(selection_path, current, project):
    try:
        selection = json.loads(Path(selection_path).read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"invalid changed test selection: {error}")
    if selection.get("schema") != CHANGED_SELECTION_SCHEMA:
        fail("unsupported changed test selection schema")
    candidates = selection.get("candidates")
    if not isinstance(candidates, list) or not candidates:
        fail("changed test selection must contain a non-empty candidates array")

    selected = {}
    metadata, _workspace_root, component_root = cargo_metadata_roots(
        project, "while resolving changed test selection"
    )
    for candidate in candidates:
        if not isinstance(candidate, dict):
            fail("changed test selection candidates must be objects")
        package = candidate.get("package")
        target_kind = candidate.get("target_kind")
        target = candidate.get("target")
        module = candidate.get("module")
        path = candidate.get("path")
        if not all(isinstance(value, str) and value for value in (package, target_kind, target)):
            fail("changed test selection candidate has an invalid package or target identity")
        if module is not None and (not isinstance(module, str) or not module):
            fail("changed test selection candidate has an invalid module")
        if isinstance(path, str) and path:
            source = (component_root / path).resolve()
            for metadata_package in metadata["packages"]:
                manifest_parent = Path(metadata_package["manifest_path"]).resolve().parent
                if source == manifest_parent or manifest_parent in source.parents:
                    package = metadata_package["name"]
                    break
        matches = matching_tests(current, package, target_kind, target, module)
        # Changed-scope candidates describe the source that led to selection;
        # their package/target fields may predate Cargo target renames or kind
        # changes. Rebind a failed candidate to Cargo's current target identity
        # by source path, preserving the exact current inventory record.
        if not matches and isinstance(path, str) and path:
            for metadata_package in metadata["packages"]:
                if metadata_package["name"] != package:
                    continue
                for metadata_target in metadata_package["targets"]:
                    kinds = metadata_target["kind"]
                    manifest_parent = Path(metadata_package["manifest_path"]).resolve().parent
                    is_lib_source = (
                        "lib" in kinds
                        and source.is_relative_to(manifest_parent / "src")
                        and not source.is_relative_to(manifest_parent / "src" / "bin")
                    )
                    if Path(metadata_target["src_path"]).resolve() != source and not is_lib_source:
                        continue
                    for current_kind in kinds:
                        current_matches = matching_tests(
                            current, package, current_kind, metadata_target["name"], module
                        )
                        if current_matches:
                            target_kind = current_kind
                            target = metadata_target["name"]
                            matches = current_matches
                            break
                    if matches:
                        break
                if matches:
                    break
        if not matches:
            return {"fallback_reason": f"Changed test selection no longer matches {package}::{target_kind}::{target}."}
        selected.update({test["id"]: test for test in matches})
    return {"selected": [selected[key] for key in sorted(selected)]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--project", required=True)
    parser.add_argument("--runner", choices=("cargo", "nextest"), required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--manifest")
    parser.add_argument("--changed-selection-file")
    parser.add_argument("--inventory-only", action="store_true")
    args = parser.parse_args()
    if args.manifest and args.changed_selection_file:
        fail("shard manifest and changed test selection are mutually exclusive")
    if args.inventory_only and args.manifest:
        fail("inventory-only mode does not accept a shard manifest")
    current = inventory(args.project, args.runner)
    output = current
    if args.manifest:
        output = {"inventory": current, "selected": validate(args.manifest, current)}
    if args.changed_selection_file:
        resolved = resolve_changed_selection(args.changed_selection_file, current, args.project)
        if args.inventory_only:
            output = current if "fallback_reason" in resolved else project_inventory(current, resolved["selected"])
        else:
            output = {"inventory": current, **resolved}
    Path(args.output).write_text(json.dumps(output, indent=2) + "\n")


if __name__ == "__main__":
    main()
