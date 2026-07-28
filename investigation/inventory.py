#!/usr/bin/env python3
"""Full inventory: every distinct test that ever printed `... FAILED`, split by
whether the phase it failed in completed or was killed at the 1500 s budget."""
import collections
import json
import os
import re

LOGDIR = os.path.dirname(os.path.abspath(__file__))
JOBDIR = os.path.join(LOGDIR, "jobs")
BASELINE_MARK = "for differential gating"
TIMEOUT_MARK = "Homeboy command timed out after"
LINE = re.compile(r"^\S+Z test ([A-Za-z0-9_:<>\-]+) \.\.\. (ok|FAILED)\s*$")

runs = json.load(open(os.path.join(LOGDIR, "harvest_all.json")))
runs.sort(key=lambda r: r["createdAt"])

# obs[test] = list of (createdAt, run, phase, killed, outcome)
obs = collections.defaultdict(list)
phase_total = {"killed": 0, "completed": 0}

for r in runs:
    log = os.path.join(JOBDIR, f"{r['run']}-{r['job']}.log")
    if not os.path.exists(log):
        continue
    phases = [{"seen": {}, "killed": False, "any": False},
              {"seen": {}, "killed": False, "any": False}]
    ph = 0
    for line in open(log, errors="replace"):
        if BASELINE_MARK in line:
            ph = 1
            continue
        if TIMEOUT_MARK in line:
            phases[ph]["killed"] = True
            continue
        m = LINE.match(line)
        if m:
            phases[ph]["any"] = True
            prev = phases[ph]["seen"].get(m.group(1))
            # a name seen twice in one phase keeps the worst outcome
            if prev != "FAILED":
                phases[ph]["seen"][m.group(1)] = m.group(2)
    for i, p in enumerate(phases):
        if not p["any"]:
            continue
        phase_total["killed" if p["killed"] else "completed"] += 1
        for name, outcome in p["seen"].items():
            obs[name].append((r["createdAt"], r["run"],
                              "candidate" if i == 0 else "baseline",
                              p["killed"], outcome))

print(f"runs examined: {len(runs)}   phases examined: "
      f"{phase_total['completed']} completed + {phase_total['killed']} killed = "
      f"{sum(phase_total.values())}")
print(f"window: {runs[0]['createdAt']} .. {runs[-1]['createdAt']}")
print(f"distinct test names observed: {len(obs)}")
print()

rows = []
for name, o in obs.items():
    fails = [x for x in o if x[4] == "FAILED"]
    if not fails:
        continue
    comp = [x for x in o if not x[3]]
    kill = [x for x in o if x[3]]
    rows.append({
        "name": name,
        "run_phases": len(o),
        "fail": len(fails),
        "fail_completed": sum(1 for x in comp if x[4] == "FAILED"),
        "obs_completed": len(comp),
        "fail_killed": sum(1 for x in kill if x[4] == "FAILED"),
        "obs_killed": len(kill),
        "first_fail": min(x[0] for x in fails),
        "last_fail": max(x[0] for x in fails),
        "last_obs": max(x[0] for x in o),
        "last_outcome": max(o, key=lambda x: x[0])[4],
    })
rows.sort(key=lambda r: (-r["fail"], r["name"]))

hdr = (f"{'test':78s} {'fail/obs':>9s} {'completed':>11s} {'killed':>9s} "
       f"{'first fail':>12s} {'last fail':>12s} {'last obs':>12s}")
print(hdr)
print("-" * len(hdr))
for r in rows:
    print(f"{r['name'][:78]:78s} {r['fail']:4d}/{r['run_phases']:<4d} "
          f"{r['fail_completed']:5d}/{r['obs_completed']:<5d} "
          f"{r['fail_killed']:4d}/{r['obs_killed']:<4d} "
          f"{r['first_fail'][5:16]:>12s} {r['last_fail'][5:16]:>12s} "
          f"{r['last_obs'][5:16]:>12s}={r['last_outcome']}")

json.dump(rows, open(os.path.join(LOGDIR, "inventory.json"), "w"), indent=1)
