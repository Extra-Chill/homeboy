# Recorded gate verdicts

`homeboy agent-task gate <cook-id> gate-1` runs the first public shell gate
from the Cook's persisted policy in the current directory. Use the candidate
workspace, not the promotion destination. Private gates remain controller-owned.
The command uses the recorded environment, package requirements and deadlines.

The shared process executor stores verdicts below Homeboy's data directory in
`controller-state/gate-verdicts/<key>/`. The key hashes the candidate working-tree
content (including untracked, non-ignored files), argv, declared test plan,
environment policy, resolved process environment and recorded input provenance.
Environment values are hashed, not stored in the key. Isolated scratch locations
are normalized; explicitly preserved host locations are not.

A matching successful verdict avoids launching the gate process. Its report and
normalized gate evidence include `verdict.reused`, `original_run` (the original
durable JSON result), `elapsed_ms`, and `saved_ms`. Failures are recorded but
never satisfy a gate. Changed inputs miss. Mutating gates cannot publish passes.
Unfingerprinted linked extension inputs deliberately disable reuse. Non-Git
workspaces continue executing gates without content-addressed reuse.

The cache lives on the execution host. Lab gates still perform admission,
materialization and setup; their process verdict can be reused only where the
same Homeboy data root and exact input identity are available. Ephemeral runner
data roots do not share verdicts. There is no controller-side Lab receipt cache.

Deleting `controller-state/gate-verdicts` forces execution again. This feature
does not make arbitrary shell commands hermetic: gates must declare inputs and
must not rely on undeclared mutable network or filesystem state.
