# #17432 reopened: previous-release ownership compatibility

## Read-only lab reproduction

Read the issue and its sole comment (6677480611, reopening after #17437).
No stop, restart, upgrade, signal, or write to the shared daemon was performed.

On this host, `~/.local/bin/homeboy daemon status` (0.446.1) reported:

- lease schema `homeboy.daemon.session_lease.v1`, PID 3478587,
  address `127.0.0.1:44435`, build `0.445.4+faff8cfc6be8c76a6dd249b94546dd9a6e084005`
- `running: false`, `reachable: false`, owner lock `held` with `holder: null`
- stale reason `pid_dead`, summary `daemon process is dead`

The pinned binary at
`/home/chubes/Developer/_chubes_net_vps/_homeboy_binaries/homeboy-6d840fb552c0baf2ecb5f50da108d8833f645df207f8422e60a2101954c2008f/homeboy daemon status`
reported `running: true`, `reachable: true`, `admits_work: true` for the same lease.
Both statuses listed two protected live controller jobs.

`ps -o pid,stat,etime,args -p 3478587` showed `Sl`, elapsed 19:04:27,
running the pinned binary with `daemon serve --addr 127.0.0.1:0` and the
lease's startup token/state directory. `ss -ltnp` showed PID 3478587 listening
on `127.0.0.1:44435`. The owner.lock file was empty.

Unauthenticated `curl http://127.0.0.1:44435/health` returned HTTP JSON
`{"success":true,"data":{"status":"ok","version":"0.445.4",...}}`.
`data.pid` matched 3478587; `data.lease` contained the full v1 lease, including
lease ID/startup token, address, PID, state path, build identity and SHA256.
`data.build_identity` matched the lease, and `data.freshness` reported two
active jobs, `fresh:true`, `restartable:false`, `recovery_evidence:"unavailable"`.

## Rejection and fix

`validate_lease_file` accepts v1 and the live PID. The later
`read_status_at_paths` check demanded `owner_lock.holder().published(state)`.
The old daemon's empty lock cannot supply that newly introduced holder record,
so this check overwrote the validation with PidDead, making reachability false
before any health request. Missing executable identity, process start identity,
and health authentication were not the rejecting checks.

The compatibility path applies only to a live v1 lease with a held, empty
owner.lock and no holder record. A bounded, proxy-free loopback GET /health
must return the matching serving lease identity, PID, path, address, schema,
build and binary hash. Modern holder validation is unchanged. The existing
busy-old-build routing and idle replacement policy then applies normally.

## Regression validation

Strengthened `workflow_status_routes_to_previous_release_v1_lease`: it now
holds the actual empty advisory owner lock and replays the previous release's
health envelope (including its full v1 lease and freshness shape), with only
isolated test process/store/endpoint identities and active-job count substituted.
It exercises full read_status and normal LocalControllerJobClient::connect,
not just lease parsing or connect_existing_job, then routes workflow status.

- With the compatibility branch disabled (main's ownership predicate), the
  strengthened test fails with running/reachable false, holder null, PidDead,
  and the same starting-daemon summary seen on the lab.
- With the fix restored, the regression passes.
- All seven tests selected by `cargo test -p homeboy-core owner_lock --lib`
  pass, including free-lock, starting-holder, and modern published-holder cases.
- `git diff --check` passes.
