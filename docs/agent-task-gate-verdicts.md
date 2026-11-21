# Recorded gate verdicts

## Cook base admission

Before provider dispatch, Cook observes declared verification gates on its pinned
base in the candidate gate placement. It records `cook_gate_baseline` on the
initial run, including exit status, parsed failing identities and an output tail.
Identical admissions reuse durable baseline observations for the same base and
gate inputs; placements do not share observations.

The default `--baseline-policy=new-failures-only` accepts candidate failures that
are a subset of the base failures. When a required gate fails, Cook replays it
on the immutable base automatically (cached per base SHA and gate inputs), so no
flag is needed to recover from a gate that is already red on trunk.

Each conclusive comparison records `baseline_comparison.failure_attribution` in
the gate receipt: `classification` is `gate_already_red_on_baseline` when every
candidate failure also fails on the base, or `candidate_broke_gate` otherwise,
with the failing identities listed in `inherited_failures` and `new_failures`.
Only new failures block; inherited failures keep their failed exit code and the
gate is reported as an accepted inherited failure.

Subset acceptance requires comparable failure identities on both sides. If
neither run names its failures, a red base and red candidate are reported as
`baseline_red`. If only one side names failures, the runs are not comparable
and the candidate failure blocks as `candidate_broke_gate`.
`--baseline-policy=strict` stops before provider dispatch when a base gate is red.
Repro gates retain their separate requirement to demonstrate the issue on base.

## Candidate verdict reuse

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
