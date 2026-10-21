#!/usr/bin/env python3
"""Check real `cargo nextest list --message-format json` profile inventories."""
import json
import sys


def inventory(path):
    with open(path, encoding="utf-8") as stream:
        data = json.load(stream)
    return data["rust-suites"]


def exclusion(identity):
    _, binary, name = identity
    if binary in ("execution_step_chaos", "cook_lab_handoff_test"):
        return binary
    if name.startswith("daemon_chaos::"):
        return "daemon_chaos"
    if name == "repository_cleanliness::repository_cleanliness_fixture_cook_leaves_only_the_provider_edit":
        return "repository_cleanliness"
    if name == "git::subtree::tests::split_budget_allows_real_split_to_run_past_previous_thirty_second_limit":
        return "subtree_split"
    return None


def main():
    ci_suites, nightly_suites = map(inventory, sys.argv[1:])
    assert ci_suites.keys() == nightly_suites.keys(), "binary inventories differ"
    ci, nightly = {}, {}
    for suite_id, suite in nightly_suites.items():
        candidate = ci_suites[suite_id]
        skipped = candidate["status"] == "skipped-default-filter"
        if skipped:
            assert suite["binary-name"] in ("execution_step_chaos", "cook_lab_handoff_test")
            assert not candidate["testcases"]
        else:
            assert candidate["testcases"].keys() == suite["testcases"].keys()
        for name, case in suite["testcases"].items():
            identity = (suite_id, suite["binary-name"], name)
            nightly[identity] = case["filter-match"]["status"]
            ci[identity] = ("mismatch" if skipped else
                            candidate["testcases"][name]["filter-match"]["status"])
    counts = dict.fromkeys(("execution_step_chaos", "cook_lab_handoff_test",
                            "daemon_chaos", "repository_cleanliness", "subtree_split"), 0)
    for identity, status in ci.items():
        group = exclusion(identity)
        suite_id, _, name = identity
        ignored = nightly_suites[suite_id]["testcases"][name]["ignored"]
        assert nightly[identity] == ("mismatch" if ignored else "matches"), (
            "nightly excluded a runnable test", identity)
        if group:
            assert status == "mismatch", ("CI selected a slow test", identity)
            counts[group] += 1
        else:
            assert status == nightly[identity], ("CI excluded an unrelated test", identity)
    assert all(counts.values()), ("stale or missing exclusion", counts)
    print(f"Verified {len(ci)} identities; CI exclusions: {counts}")


if __name__ == "__main__":
    main()
