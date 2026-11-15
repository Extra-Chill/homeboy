# Daemon Lifecycle

A maintainer's map of how the local daemon decides it is alive, who owns which
jobs, and how a stale daemon hands over to its replacement. Operator behavior
(lifetimes, service managers, recovery commands) is documented in
[`homeboy daemon`](../../commands/daemon.md); this page names the code that
owns each decision.

## Files in the state directory

One daemon serves one state directory (`HOMEBOY_DAEMON_STATE_DIR`, normally
`~/.config/homeboy/daemon/`):

| File | Written by | Meaning |
| --- | --- | --- |
| `owner.lock` | the serving daemon | Held for the daemon's whole life. Records the holder's PID, process start identity, and startup token. |
| `state.json` | the serving daemon, while holding `owner.lock` | The lease: lease ID, PID, build identity, endpoint, service manager. |
| `state.drain` | `daemon recover --drain` | The lease ID being drained on request. |
| `admission.lock` | admissions and lifecycle commands | Fences admission while a lifecycle change runs. |
| `termination.json` | `serve` and `daemon stop` | The last termination evidence. |

## Decisions and their owners

Each question below has one owner. Change the behavior there, and have callers
ask it rather than re-deriving the answer.

| Question | Owner |
| --- | --- |
| Is the daemon alive, fresh, reachable? | `read_status_at_paths` (`daemon/mod.rs`) builds the one `DaemonStatus`. |
| Is the lease's daemon really alive? | The owner lock. A lease its holder did not publish is dead, even if its PID was reused. |
| Which ledger jobs does this daemon answer for? | `JobStore::answers_for` and `active_daemon_job_recovery_evidence` (`api_jobs/store/reconciliation.rs`). |
| Can it admit new root work? | `DaemonStatus::admits_work`. |
| Is it draining? | `DaemonStatus::draining` for status readers; `intake_draining` inside the serving daemon. |
| What should recovery do next? | `recovery_actions::plan_recovery`, named on the wire as a `lifecycle_plan::Plan`. |
| Which recovery commands exist? | `recovery_actions`, which defines each `homeboy daemon *` invocation once, as argv. |
| When does an idle daemon stop itself? | `lifetime.rs`. |
| How are HTTP requests routed? | `router::dispatch` (`daemon/router.rs`). |

## Liveness

Liveness comes from `owner.lock`, not from the lease PID. Only `serve` writes
the lease, and only while it holds the lock. So a free lock with a present
`state.json` proves the lease writer is dead, and a lock held without a live
lease blocks replacement. Homeboy never scans the process table for daemon
candidates.

## Job ownership in a shared ledger

Every daemon run by one Unix user shares one job ledger, whatever its state
directory. A daemon answers for a job in its own daemon scope that carries its
lease, or that carries no lease (no proven owner). A state directory with no
lease answers only for jobs with no lease. A dead generation's in-scope job
awaiting attestation is always reported, because only this scope can attest
it. Those answered-for jobs, and nothing else, count against capacity and
protect the daemon from replacement.

## Draining and replacement

A daemon is draining when either:

- its build is stale (version, build identity, or replaced executable) and it
  still has active jobs, or
- `daemon recover --drain` wrote its lease ID to `state.drain`.

While draining, the daemon serves only work it already owns. A POST to `/jobs`,
`/exec`, `/runner/jobs`, `/admissions`, or `/v1/control-plane/runs` is admitted
only if it continues a durable run this lease owns; anything else is refused
with `daemon_draining`. A new controller job is admitted durably and waits for
the replacement. Status reports `waiting_for: daemon_replacement`.

Once its owned jobs finish, the draining daemon idles out. On that self-stop it
releases its lease and owner lock, then launches `daemon ensure-running` from
the installed binary if admissions or workflows are waiting. A service manager,
when one owns the daemon, starts the replacement instead. Upgrade convergence
leaves a draining daemon alone for the same reason.

## Recovery

`plan_recovery` reads one `DaemonStatus` and returns both the executable
repair steps and the next lifecycle action: `Blocked`, `Wait`, `Draining`,
`NeedsAttestation`, `Transition`, or `Converged`. `daemon plan`, `daemon
recover`, status repair text, and upgrade convergence all follow that one
verdict. A replacement daemon recovers its dead predecessor's controller jobs
at startup; jobs with no recorded child identity wait for `daemon recover
--attest-absent`.
