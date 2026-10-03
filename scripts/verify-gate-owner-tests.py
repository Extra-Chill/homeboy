"""Record native owner checks, including broader failures without waiving them."""
import json
from pathlib import Path
import subprocess
import sys

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
checks = [
    ("gate-protocol", "homeboy-agents", "agent_task_gate::placement::tests::"),
    ("gate-cli-parser", "homeboy-cli", "admitted_placement_tests"),
    ("promotion-lab-binding", "homeboy-agents", "lab_gate_binding_tests"),
    ("external-installation-source-owner", "homeboy-core", "declared_external_installation_shared_sources_and_writes_ignore_different_ambient_config"),
    ("runtime-generation-write-boundary", "homeboy-core", "declared_source_install_preserves_runtime_generation_write_boundary"),
    ("private-at-file-owner", "homeboy-lab-runner", "private_at_file"),
    ("daemon-status-owner", "homeboy-lab-runner", "remote_daemon_status_"),
    ("broader-gate-owner", "homeboy-agents", "agent_task_gate::tests::"),
]
results = []
for name, package, selector in checks:
    command = ["cargo", "test", "--quiet", "-p", package, "--lib", selector,
               "--", "--test-threads=1"]
    completed = subprocess.run(command, capture_output=True, text=True)
    (root / f"{name}.stdout.txt").write_text(completed.stdout)
    (root / f"{name}.stderr.txt").write_text(completed.stderr)
    results.append({"name": name, "command": command, "exit_code": completed.returncode})
    print(name + ": " + completed.stdout, flush=True)
(root / "commands-and-outcomes.json").write_text(json.dumps(results, indent=2))
sys.exit(1 if any(result["exit_code"] for result in results) else 0)
