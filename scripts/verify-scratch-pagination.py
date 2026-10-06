#!/usr/bin/env python3
"""Exercise scratch pagination and apply safety through the real CLI binary."""

import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="scratch-pagination-") as temporary:
        root = Path(temporary)
        data = root / "data"
        scratch = data / "controller-scratch"
        leases = scratch / "attempts"
        leases.mkdir(parents=True)
        resources = []
        protected = set()
        now = datetime.datetime.now(datetime.timezone.utc).isoformat()
        for number in range(65):
            path = leases / f"lease-{number:04}"
            path.mkdir()
            (path / "evidence").write_text(f"Evidence for lease {number}\n")
            live = number % 7 == 0
            unmanaged = number % 11 == 0
            if live or unmanaged:
                protected.add(str(path))
            resources.append({
                "path": str(path), "run_id": f"terminal-{number}",
                "plan_id": "pagination-proof", "task_id": f"task-{number}",
                "attempt": 1, "root_bound": str(leases),
                "owner_pid": os.getpid() if live else 2147483647,
                "lifecycle_state": "released", "lease_id": f"lease-{number}",
                "reconstructable": not unmanaged, "ephemeral": True,
                "retention": "0s", "created_at": now, "finalized_at": now,
                "terminal_reason": "verified-fixture",
            })
        (scratch / "resources.json").write_text(json.dumps({
            "schema": "homeboy/controller-scratch/v1", "resources": resources,
        }))
        env = dict(os.environ)
        env.update({
            "HOME": str(root / "home"), "HOMEBOY_DATA_DIR": str(data),
            "HOMEBOY_CONFIG_DIR": str(root / "config"),
            "XDG_CONFIG_HOME": str(root / "xdg-config"),
            "XDG_DATA_HOME": str(root / "xdg-data"),
            "XDG_CACHE_HOME": str(root / "xdg-cache"),
            "XDG_STATE_HOME": str(root / "xdg-state"),
        })
        timings = []
        for apply in (False, True):
            cursor = None
            seen = set()
            inspected = removed = skipped = pages = 0
            while True:
                command = [str(binary), "--placement", "local", "cleanup",
                           "--include", "controller-scratch", "--limit", "5"]
                if cursor:
                    command += ["--cursor", cursor]
                if apply:
                    command.append("--apply")
                started = time.monotonic()
                result = subprocess.run(command, env=env, cwd=root,
                                        capture_output=True, text=True, timeout=20)
                timings.append(time.monotonic() - started)
                envelope = json.loads(result.stdout)
                category = envelope["data"]["categories"][0]
                assert category.get("failure") is None, (command, result.stdout, result.stderr)
                output = category["output"]
                assert output["inspected_count"] <= 5, output
                assert category["inventory_completeness"] == (
                    "partial" if output["next_cursor"] else "complete"), category
                paths = {row["path"] for row in output["candidates"]}
                assert not paths.intersection(seen), "candidate repeated across pages"
                seen.update(paths)
                inspected += output["inspected_count"]
                removed += output["applied_count"]
                skipped += output["skipped_count"]
                pages += 1
                next_cursor = output["next_cursor"]
                if not next_cursor:
                    break
                assert next_cursor != cursor, "continuation did not advance"
                assert next_cursor in category["continuation_command"], category
                cursor = next_cursor
            assert inspected == 65, inspected
            assert seen == {row["path"] for row in resources} - protected, seen
            assert skipped == len(protected), skipped
            assert removed == (65 - len(protected) if apply else 0), removed
            remaining = {str(path) for path in leases.iterdir()}
            assert remaining == (protected if apply else {row["path"] for row in resources}), remaining
            print(json.dumps({"phase": "apply" if apply else "preview", "pages": pages,
                              "inspected": inspected, "removed": removed,
                              "protected": len(protected)}), flush=True)
        print(json.dumps({"max_page_seconds": round(max(timings), 3),
                          "total_page_seconds": round(sum(timings), 3)}), flush=True)


if __name__ == "__main__":
    main()
