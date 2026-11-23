# Recorded gate verdicts

## Local host admission

Local gates on one host share an admission queue across every process using
the same Homeboy data root. `HOMEBOY_LOCAL_GATE_SLOTS` sets a positive slot limit
(default: half the available CPUs, rounded down, minimum one).
`HOMEBOY_LOCAL_GATE_MAX_LOAD` sets a positive one-minute load threshold (default:
available CPUs). A gate waits while every slot is held, or while host load is at
the threshold and another local gate already runs (one gate always progresses).
Hosts without `/proc/loadavg` still enforce slots; capacity reports the missing
load. Waiters rank like the Lab step queue: `HOMEBOY_LAB_PRIORITY` lane, then the
least-occupied owner (the `HOMEBOY_WORKFLOW_INSTANCE` when set, else the Cook),
then arrival, with the `HOMEBOY_LAB_OWNER_MAX_SHARE` cap while another owner
waits. `HOMEBOY_LAB_BUDGET_SECONDS` feeds the upper-bound ETA.

Queue wait is recorded as `environment.admission_wait_ms` and excluded from gate
execution and no-progress budgets. A Cook cancelled while queued records a
`cancelled` non-execution gate, never candidate blame. Dead processes release
their slots through OS file locks, reaped on the next admission or capacity read.

On a Lab host, the daemon charges local gate holders against both of its
admission pools. Step admission counts local gates as occupied slots against
`HOMEBOY_LAB_CONCURRENCY_LIMIT` (or one per CPU); the reported effective budget
stays unchanged. Runner-child capacity retains its existing local-holder
accounting. Load does not reduce the step budget. A Cook the daemon already
supervises (a workflow step or runner
child) holds a daemon slot, so its gates queue for host slots but are not charged
again. Runner-admitted Lab gates keep the daemon gate lane and never enter the
local queue. Charging requires the daemon and the local Cooks to share one
Homeboy data root.

`agent-task capacity` reports `local_capacity` without daemon transport: slots,
holders (Cook, gate index, elapsed), pressure, `lab_charged_holders`, and ranked
`waiting` entries with `queue_position`, `priority`, `waited_seconds` and
`eta_seconds`, the fields the Lab step queue publishes. `agent-task status`
reports `local_admission` for a queued Cook. Private command text is never
published in these views.

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
with complete `base_failures` and `head_failures` lists and the set difference
in `new_failures` (the intersection is `inherited_failures`).
Only new failures block; inherited failures keep their failed exit code and the
gate is reported as an accepted inherited failure.

When the initial comparison would blame the candidate, Cook performs one extra
run on each side under the same timeout and recorded gate inputs. Only failures
present in both candidate runs and absent from the union of baseline failures
block promotion. `failure_attribution.flaky_failures` lists identities seen in
only one run on either side; these never block on their own. For example, base
`{a,b}` then `{a}` and candidate `{a,b,c}` then `{a,c}` produces inherited `{a}`,
new `{c}`, and flaky `{b}`. An opaque candidate that passes its extra run is
explicitly recorded as flaky-passing, with its original failure evidence retained.
Replay errors or timeouts remain inconclusive and cannot excuse a failed gate.

Subset acceptance requires comparable failure identities on both sides. If
neither run names its failures, matching normalized failure fingerprints are
required for `baseline_red`; exit status alone is not proof. The fingerprints
are retained in the base/head lists. If only one side names failures, the runs are not comparable
and the candidate failure blocks as `candidate_broke_gate`.
`--baseline-policy=strict` stops before provider dispatch when a base gate is red.
Repro gates retain their separate requirement to demonstrate the issue on base.

Cook reports, control-plane status/list summaries, and shared CLI verdicts expose
an `inherited_failures` count of accepted red gates. Completion notifications name
that count explicitly as inherited, not passing. Lifecycle success remains success;
this additive evidence distinguishes it from an entirely green verification.

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
