#!/usr/bin/env python3
"""Harvest per-phase test failures from homeboy CI *job* logs.

Why job logs and not `gh run view <id> --log`: the run-level log zip truncates
individual job logs. Measured on run 30380078587 the Test job was 15,531 lines
via the run zip and 26,353 lines via
/repos/{owner}/{repo}/actions/jobs/{id}/logs -- the run zip silently dropped
the entire baseline phase, including a second occurrence of the same failure.

Method
------
For each CI run we locate the job literally named "homeboy / Test" and fetch its
log. The Homeboy composite action runs the suite twice; the line

    Checking out baseline ref <sha> for differential gating

separates phase 0 (candidate, the PR head) from phase 1 (baseline, the merge
base -- i.e. main). Runs whose candidate phase is clean never emit that line and
therefore have no phase 1.

A failing test is a line matching `test <name> ... FAILED` anchored to end of
line. libtest emits that line when, and only when, a test completes with a
failure, so each such line is a genuine failure even if the harness is later
killed. What a kill destroys is the *summary* (`test result:`) and the sidecar
counts -- so a killed phase undercounts and must be reported separately.
"""
import json
import os
import re
import subprocess
import sys

REPO = "Extra-Chill/homeboy"
LOGDIR = os.path.dirname(os.path.abspath(__file__))
JOBDIR = os.path.join(LOGDIR, "jobs")

FAILED_RE = re.compile(r"^\S+Z test ([A-Za-z0-9_:<>\-]+) \.\.\. FAILED\s*$")
OK_RE = re.compile(r"^\S+Z test ([A-Za-z0-9_:<>\-]+) \.\.\. ok\s*$")
RESULT_RE = re.compile(r"Z test result: (ok|FAILED)\. (\d+) passed; (\d+) failed")
BASELINE_MARK = "for differential gating"
TIMEOUT_MARK = "Homeboy command timed out after"
GATE_RE = re.compile(r"Differential gate (\w+) (.+)$")
BASE_EXIT_RE = re.compile(r"Baseline homeboy (.+?) exited (\d+)")


def new_phase():
    return {"failed": [], "ok": 0, "timeout": False,
            "result_ok": 0, "result_failed": 0, "harness_exit_124": False}


def analyse(path):
    phases = [new_phase(), new_phase()]
    gate_lines, base_exit = [], []
    phase = 0
    with open(path, "r", errors="replace") as fh:
        for line in fh:
            if BASELINE_MARK in line:
                phase = 1
                continue
            p = phases[phase]
            m = FAILED_RE.match(line)
            if m:
                p["failed"].append(m.group(1))
                continue
            if OK_RE.match(line):
                p["ok"] += 1
                continue
            if TIMEOUT_MARK in line:
                p["timeout"] = True
                continue
            if '"exit_code": 124' in line:
                p["harness_exit_124"] = True
            m = RESULT_RE.search(line)
            if m:
                if m.group(1) == "ok":
                    p["result_ok"] += 1
                else:
                    p["result_failed"] += 1
                continue
            m = GATE_RE.search(line)
            if m:
                gate_lines.append(m.group(0).strip())
                continue
            m = BASE_EXIT_RE.search(line)
            if m:
                base_exit.append(m.group(0).strip())
    return phases, gate_lines, base_exit


def main():
    os.makedirs(JOBDIR, exist_ok=True)
    runs = json.load(open(os.path.join(LOGDIR, "runs.json")))
    done = [r for r in runs if r["status"] == "completed"
            and r["conclusion"] in ("success", "failure")]
    limit = int(sys.argv[1]) if len(sys.argv) > 1 else 40
    out = []
    for r in done[45:limit]:
        rid = r["databaseId"]
        meta_path = os.path.join(JOBDIR, f"{rid}.jobs.json")
        if not os.path.exists(meta_path):
            pr = subprocess.run(["gh", "run", "view", str(rid), "--repo", REPO,
                                 "--json", "jobs"], capture_output=True, text=True)
            if pr.returncode != 0:
                print(f"SKIP {rid}: jobs fetch failed", file=sys.stderr)
                continue
            open(meta_path, "w").write(pr.stdout)
        jobs = json.load(open(meta_path))["jobs"]
        tj = [j for j in jobs if j["name"] == "homeboy / Test"]
        if not tj:
            print(f"SKIP {rid}: no Test job", file=sys.stderr)
            continue
        job = tj[0]
        jid = job["databaseId"]
        log = os.path.join(JOBDIR, f"{rid}-{jid}.log")
        if not os.path.exists(log):
            with open(log, "wb") as fh:
                pr = subprocess.run(["gh", "api",
                                     f"/repos/{REPO}/actions/jobs/{jid}/logs"],
                                    stdout=fh, stderr=subprocess.DEVNULL)
            if pr.returncode != 0 or os.path.getsize(log) == 0:
                print(f"SKIP {rid}: job log fetch failed", file=sys.stderr)
                os.remove(log)
                continue
        phases, gate_lines, base_exit = analyse(log)
        out.append({
            "run": rid, "job": jid, "conclusion": r["conclusion"],
            "test_job_conclusion": job["conclusion"],
            "sha": r["headSha"], "createdAt": r["createdAt"],
            "title": r["displayTitle"], "phases": phases,
            "gate": gate_lines, "baseline_exit": base_exit,
        })
        print(f"done {rid}", file=sys.stderr)
    json.dump(out, open(os.path.join(LOGDIR, "harvest2.json"), "w"), indent=1)
    print(f"wrote {len(out)} runs", file=sys.stderr)


if __name__ == "__main__":
    main()
