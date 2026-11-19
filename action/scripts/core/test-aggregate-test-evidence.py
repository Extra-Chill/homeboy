#!/usr/bin/env python3
"""Aggregate shard evidence whose failures are finer than planned units (#18425).

File-granular runners (WordPress PHPUnit) shard whole test files but report
failures as `Class::method`. Each shard binds those failures to its own file
units through `failed_test_units`; the aggregate keeps the precise failed IDs
and the bindings, and rejects bindings that leave the shard's inventory.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

AGGREGATE = Path(__file__).with_name("aggregate-test-evidence.py")
COMMAND = "review test"
PROVENANCE = {"runner": "phpunit", "runner_fingerprint": "a" * 64, "workspace_fingerprint": "b" * 64}
SOURCE_INVENTORY = "d" * 64


def sha(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def shard_pair(root: Path, shard_id: str, tests: list[str], failures: dict[str, str | None]) -> dict:
    execution = sha({
        "inventory_fingerprint": SOURCE_INVENTORY,
        "runner": PROVENANCE["runner"],
        "runner_fingerprint": PROVENANCE["runner_fingerprint"],
        "source": "shard_manifest",
        "source_id": shard_id,
        "tests": sorted(tests),
        "workspace_fingerprint": PROVENANCE["workspace_fingerprint"],
    })
    provenance = {**PROVENANCE, "execution_fingerprint": execution}
    inventory = {
        "command": COMMAND,
        **provenance,
        "schema": "homeboy/test-inventory/v1",
        "tests": [{"id": identity} for identity in sorted(tests)],
    }
    inventory["inventory_fingerprint"] = sha(inventory)
    outcomes = {
        "schema": "homeboy/test-outcomes/v1",
        "command": COMMAND,
        **provenance,
        "inventory_fingerprint": inventory["inventory_fingerprint"],
        "failed_test_ids": sorted(failures),
    }
    units = {identity: unit for identity, unit in failures.items() if unit is not None}
    if units:
        outcomes["failed_test_units"] = units
    inventory_path = root / f"{shard_id}.test-inventory.json"
    outcomes_path = root / f"{shard_id}.test-outcomes.json"
    inventory_path.write_text(json.dumps(inventory), encoding="utf-8")
    outcomes_path.write_text(json.dumps(outcomes), encoding="utf-8")
    return {
        "id": shard_id,
        "status": "fail" if failures else "pass",
        "failed_count": len(failures),
        "inventory": str(inventory_path),
        "outcomes": str(outcomes_path),
    }


def aggregate(root: Path, shards: dict[str, tuple[list[str], dict[str, str | None]]]) -> subprocess.CompletedProcess:
    plan = {"plan_digest": "e" * 64, "shards": []}
    records = []
    for shard_id, (tests, failures) in shards.items():
        plan["shards"].append({"id": shard_id, **PROVENANCE, "inventory_fingerprint": SOURCE_INVENTORY, "tests": tests})
        records.append(shard_pair(root, shard_id, tests, failures))
    parent = {**PROVENANCE, "tests": [{"id": test} for tests, _ in shards.values() for test in tests]}
    (root / "plan.json").write_text(json.dumps(plan), encoding="utf-8")
    (root / "parent.json").write_text(json.dumps(parent), encoding="utf-8")
    output = root / "output"
    output.mkdir(exist_ok=True)
    return subprocess.run(
        ["python3", str(AGGREGATE), str(root / "plan.json"), str(root / "parent.json"), COMMAND, str(output)],
        input="".join(json.dumps(record) + "\n" for record in records),
        text=True,
        capture_output=True,
    )


def main() -> None:
    foo, bar, baz = "tests/unit/FooTest.php", "tests/unit/BarTest.php", "tests/unit/BazTest.php"
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        completed = aggregate(root, {
            "shard-1": ([foo, bar], {"Foo_Test::test_a": foo, "Foo_Test::test_b": foo}),
            "shard-2": ([baz], {"Baz_Test::test_c": baz}),
        })
        assert completed.returncode == 0, completed.stderr
        outcomes = json.loads((root / "output" / "review-test.test-outcomes.json").read_text(encoding="utf-8"))
        assert outcomes["failed_test_ids"] == ["Baz_Test::test_c", "Foo_Test::test_a", "Foo_Test::test_b"], outcomes
        assert outcomes["failed_test_units"] == {
            "Baz_Test::test_c": baz,
            "Foo_Test::test_a": foo,
            "Foo_Test::test_b": foo,
        }, outcomes
        inventory = json.loads((root / "output" / "review-test.test-inventory.json").read_text(encoding="utf-8"))
        assert [test["id"] for test in inventory["tests"]] == [bar, baz, foo], inventory

    # Exact-ID shards keep the original schema: no unit mapping is emitted.
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        completed = aggregate(root, {"shard-1": ([foo, bar], {foo: None})})
        assert completed.returncode == 0, completed.stderr
        outcomes = json.loads((root / "output" / "review-test.test-outcomes.json").read_text(encoding="utf-8"))
        assert outcomes["failed_test_ids"] == [foo] and "failed_test_units" not in outcomes, outcomes

    for failures, why in (
        # The unit lives in another shard: a shard may only bind to its own plan.
        ({"Baz_Test::test_c": baz}, "a unit outside the shard"),
        ({"Foo_Test::test_a": None}, "an unbound method failure"),
        ({foo: foo}, "a mapping that shadows an inventory identity"),
    ):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            completed = aggregate(root, {"shard-1": ([foo, bar], failures), "shard-2": ([baz], {})})
            assert completed.returncode == 1, f"{why} must fail closed"
            assert "unknown failures" in completed.stderr, completed.stderr

    print("PASS: shard aggregation binds method failures to file units")


if __name__ == "__main__":
    main()
