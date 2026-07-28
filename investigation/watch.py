#!/usr/bin/env python3
"""Per-run/per-phase outcome for a watchlist of tests, with kill status."""
import json
import os
import re

LOGDIR = os.path.dirname(os.path.abspath(__file__))
JOBDIR = os.path.join(LOGDIR, "jobs")
BASELINE_MARK = "for differential gating"
TIMEOUT_MARK = "Homeboy command timed out after"
LINE = re.compile(r"^\S+Z test ([A-Za-z0-9_:<>\-]+) \.\.\. (ok|FAILED)\s*$")

WATCH = [
    "agent_task_gate::tests::existing_gate_commands_are_automatic_toolchain_requirements",
    "agent_task_gate::baseline_tests::bounded_baseline_gate_is_cancelled",
    "agent_task_gate::baseline_tests::bounded_baseline_gate_reaps_background_descendants_before_reader_join",
    "agent_task_lifecycle::activity_provider::tests::probe_by_id_resolves_one_record_without_scanning_or_writing",
    "agent_task_lifecycle::activity_provider::tests::show_activity_resolves_an_agent_task_id_through_the_indexed_probe",
    "agent_task_dispatch_plan::tests::dispatch_plan_preflight_fails_on_unresolved_runtime_dependency",
    "agent_task_finalization::tests::durable_finalization_uses_promoted_files_for_clean_committed_candidate",
    "agent_task_finalization::tests::production_validator_accepts_only_the_exact_promoted_recovery_commit",
    "agent_task_dispatch_plan::tests::dispatch_plan_preserves_abstract_executor_requirements",
    "read_only_cli_commands_complete_while_runtime_promotion_is_held",
    "detached_cook_accepts_reverse_capacity_queue_and_worker_completes_once",
]

runs = json.load(open(os.path.join(LOGDIR, "harvest2.json")))
rows = []
for r in runs:
    log = os.path.join(JOBDIR, f"{r['run']}-{r['job']}.log")
    phases = [{"seen": {}, "timeout": False, "any": False},
              {"seen": {}, "timeout": False, "any": False}]
    ph = 0
    for line in open(log, errors="replace"):
        if BASELINE_MARK in line:
            ph = 1
            continue
        if TIMEOUT_MARK in line:
            phases[ph]["timeout"] = True
            continue
        m = LINE.match(line)
        if m:
            phases[ph]["any"] = True
            if m.group(1) in WATCH:
                phases[ph]["seen"][m.group(1)] = m.group(2)
    for i, p in enumerate(phases):
        if not p["any"]:
            continue
        rows.append({"run": r["run"], "at": r["createdAt"], "sha": r["sha"][:8],
                     "phase": "candidate" if i == 0 else "baseline",
                     "killed": p["timeout"], "seen": p["seen"]})
rows.sort(key=lambda x: (x["at"], x["phase"]))
json.dump(rows, open(os.path.join(LOGDIR, "watch.json"), "w"), indent=1)

for t in WATCH:
    obs = [r for r in rows if t in r["seen"]]
    okc = sum(1 for r in obs if r["seen"][t] == "ok")
    fc = len(obs) - okc
    kill = sum(1 for r in obs if r["killed"])
    print(f"\n### {t}")
    print(f"    observed in {len(obs)} phases ({kill} killed / {len(obs)-kill} completed): {fc} FAILED, {okc} ok")
    for r in obs:
        print(f"      {r['at'][5:16]} {r['run']} {r['phase']:9s} {'KILLED   ' if r['killed'] else 'completed'} -> {r['seen'][t]}")
