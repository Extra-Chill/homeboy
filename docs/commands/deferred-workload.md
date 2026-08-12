# Deferred workloads

`homeboy deferred-workload` resumes portable workloads that were retained until
a compatible runner becomes available.

Use `homeboy deferred-workload status` to inspect pending work and the worker.
The controller-owned worker is started with `homeboy deferred-workload worker`.

## The worker is a singleton, and proves it

Exactly one worker may run per Homeboy home. Ownership has two halves, and both
are required:

- the exclusive lock on the Homeboy config directory inode, which is the
  authority, and
- a random `--startup-token` that the worker carries in its **execve**
  environment as `HOMEBOY_DEFERRED_WORKLOAD_OWNER`, which is how another process
  proves the recorded worker pid is the worker rather than a recycled pid.

The environment half must be set by whoever spawns the worker. `/proc/<pid>/environ`
exposes the block the kernel copied at exec time, so a worker that sets the
variable on itself can never prove ownership — liveness then always reads false
and every mutating command spawns another worker (#12081). A worker started by
hand re-execs itself once to publish the marker.

The worker also runs from the Homeboy config directory, never from the directory
of the command that started it. A singleton that outlives its caller must not
hold an ephemeral worktree open; that is how workers ended up anchored to
worktrees that had since been finalized and deleted.

## Records carry the worktree they belong to

A deferred record stores the source worktree it was deferred from, and the
replay is dispatched from that directory. Two identical commands deferred from
two worktrees are therefore two records, and a record whose worktree no longer
exists fails rather than replaying against whatever directory the worker happens
to be standing in.

## Reconciling orphaned workers

`homeboy deferred-workload reconcile` terminates worker processes that no live
durable ownership backs. Use `--dry-run` to see the decision without signaling
anything.

A process is only a *candidate* because of its command line. Whether it may keep
running is decided from durable state: the record store says whether any work
remains, and the worker status plus the process's own environment say which pid
owns the singleton. Everything else is orphaned, and its claims are returned to
the queue before it is signaled so the next worker can pick them up immediately
instead of waiting out the lease.

One pass is time-bounded. Whatever the budget does not reach is reported under
`remaining`; run the command again to continue.

The generated [CLI reference](../reference/cli/commands/deferred-workload.md)
contains the complete command, argument, and flag surface.
