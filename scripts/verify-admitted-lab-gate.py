"""Native admitted-runner verification; run only through Homeboy runner exec.

Requires an already built candidate binary as argv[1]. Exercises real Rust
tests, private HOME, canonical proof and changed-candidate rejection. No daemon
or runtime configuration is changed.
"""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def run(argv, cwd=None):
    return subprocess.check_output(argv, cwd=cwd, text=True).strip()


binary = str(Path(sys.argv[1]).resolve())
runner = sys.argv[2]
evidence = Path(sys.argv[3]).resolve()
evidence.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="homeboy-admitted-gate-") as directory:
    root = Path(directory).resolve()
    (root / "src").mkdir()
    (root / "Cargo.toml").write_text(
        '[package]\nname="admitted-gate-fixture"\nversion="0.1.0"\nedition="2021"\n'
    )
    (root / "src/lib.rs").write_text(
        '#[test] fn first() { assert_ne!(std::env::var("HOMEBOY_FAIL_TEST").unwrap_or_default(), "true"); }\n'
        '#[test] fn private_home() { let home = std::env::var("HOME").unwrap(); '
        'assert_ne!(home, std::env::var("HOMEBOY_EXPECTED_OPERATOR_HOME").unwrap()); '
        'assert!(std::path::Path::new(&home).join(".config/homeboy/extensions/fixture/fixture.json").is_file()); '
        'assert_eq!(std::fs::read_to_string(std::path::Path::new(&home).join(".config/homeboy/extensions/shared/declared.txt")).unwrap(), "declared resource"); }\n'
    )
    (root / ".gitignore").write_text("target/\nCargo.lock\n")
    for args in (
        ["init", "-q"],
        ["config", "user.name", "Homeboy Native Fixture"],
        ["config", "user.email", "fixture@example.invalid"],
        ["add", "."],
        ["commit", "-qm", "native candidate"],
    ):
        subprocess.run(["git", *args], cwd=root, check=True)
    head = run(["git", "rev-parse", "HEAD"], root)
    digest = hashlib.sha256()
    for kind in (b"staged", b"unstaged"):
        digest.update(len(kind).to_bytes(8, "big"))
        digest.update(kind)
        digest.update((0).to_bytes(8, "big"))
    cargo = run(["rustup", "which", "cargo"])
    rustc = run(["rustup", "which", "rustc"])
    resources = evidence / "declared-resources"
    (resources / "fixture").mkdir(parents=True, exist_ok=True)
    (resources / "shared").mkdir(parents=True, exist_ok=True)
    (resources / "fixture/fixture.json").write_text('{"id":"fixture"}')
    (resources / "shared/declared.txt").write_text("declared resource")
    (resources / "homeboy-extension-root.json").write_text('{"shared_assets":["shared"]}')
    request = {
        "runner_id": runner,
        "candidate": {"kind": "git", "fingerprint": {
            "schema": "homeboy/agent-task-candidate-fingerprint/v1",
            "target_path": str(root), "head": head, "base": head,
            "changed_files": [], "sha256": digest.hexdigest(),
            "tree": run(["git", "rev-parse", "HEAD^{tree}"], root),
        }},
        "index": 1, "argv": [cargo, "test", "--offline", "--lib"],
        "label": "cargo test --offline --lib",
        "visibility": "visible", "reveal_policy": "full_evidence",
        "environment": {
            "mode": "replace", "variables": {
                "PATH": str(Path(rustc).parent) + ":/usr/bin:/bin",
                "RUSTC": rustc,
                "HOMEBOY_EXPECTED_OPERATOR_HOME": os.environ["HOME"],
            }, "preserve": {}, "isolate_home": True, "isolate_xdg": True,
            "hydrate_rust_cache": False, "extension_inputs": [
                {"id": "fixture", "source": str(resources / "fixture")}
            ],
        },
        "package_artifacts": [], "declared_plan": None,
        "timeout_seconds": 600, "no_progress_timeout_seconds": 600,
    }
    command = [binary, "--placement", "local", "agent-task", "gate-execute",
               "--request", json.dumps(request)]
    completed = subprocess.run(command, cwd=root, capture_output=True, text=True)
    (evidence / "passing.stdout.json").write_text(completed.stdout)
    (evidence / "passing.stderr.txt").write_text(completed.stderr)
    assert completed.returncode == 0, completed.stderr + completed.stdout
    receipt = json.loads(completed.stdout)["data"]
    assert receipt["schema"] == "homeboy/admitted-lab-gate-receipt/v1"
    assert receipt["candidate"] == request["candidate"]
    assert receipt["execution_context"]["context"]["runner_job_id"] == os.environ["HOMEBOY_RUNNER_JOB_ID"]
    report = receipt["report"]
    assert report["status"] == "succeeded", report
    assert "2 passed; 0 failed" in report["stdout"], report
    assert any(value["name"] == "HOME" for value in report["environment"]["sanitized"])
    request["environment"]["variables"]["HOMEBOY_FAIL_TEST"] = "true"
    failed = subprocess.run(command[:-1] + [json.dumps(request)], cwd=root, capture_output=True, text=True)
    (evidence / "failed.stdout.json").write_text(failed.stdout)
    (evidence / "failed.stderr.txt").write_text(failed.stderr)
    assert failed.returncode != 0
    red = json.loads(failed.stdout)["data"]["report"]
    assert red["status"] == "failed" and "1 passed; 1 failed" in red["stdout"], red
    del request["environment"]["variables"]["HOMEBOY_FAIL_TEST"]
    request["environment"]["extension_inputs"][0]["identity"] = report["environment"]["extension_inputs"][0]["identity"]
    (resources / "shared/declared.txt").write_text("altered resource")
    drift = subprocess.run(command[:-1] + [json.dumps(request)], cwd=root, capture_output=True, text=True)
    (evidence / "closure-drift.stdout.json").write_text(drift.stdout)
    (evidence / "closure-drift.stderr.txt").write_text(drift.stderr)
    assert drift.returncode != 0 and "no longer matches" in drift.stdout + drift.stderr
    unauthenticated_env = os.environ.copy()
    for name in ("HOMEBOY_RUNNER_JOB_ID", "HOMEBOY_RUNNER_CHILD_RESERVATION", "HOMEBOY_RUNNER_JOB_EXECUTION_CONTEXT_ID"):
        unauthenticated_env.pop(name, None)
    unadmitted = subprocess.run(command, cwd=root, env=unauthenticated_env, capture_output=True, text=True)
    (evidence / "unadmitted.stdout.json").write_text(unadmitted.stdout)
    assert unadmitted.returncode != 0 and "requires a live authenticated" in unadmitted.stdout + unadmitted.stderr
    (root / "src/lib.rs").write_text("// altered candidate\n")
    rejected = subprocess.run(command, cwd=root, capture_output=True, text=True)
    (evidence / "altered.stdout.json").write_text(rejected.stdout)
    (evidence / "altered.stderr.txt").write_text(rejected.stderr)
    assert rejected.returncode != 0
    assert "differs from the controller candidate" in rejected.stdout + rejected.stderr
    print(json.dumps({"schema": "homeboy/admitted-gate-native-verification/v1",
                      "rust_tests_passed": 2, "red_rust_tests_passed": 1, "red_rust_tests_failed": 1,
                      "altered_candidate_rejected": True, "closure_drift_rejected": True,
                      "unadmitted_execution_rejected": True,
                      "runner_job_id": os.environ["HOMEBOY_RUNNER_JOB_ID"]}))
