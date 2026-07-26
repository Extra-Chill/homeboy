# `homeboy schedule`

```text
homeboy schedule add <id> (--command <argv> | --exec <program> [--exec-arg <arg>]... [--working-dir <dir>])
                         --every <interval> [--notify-on <policy>] [--on-overlap <policy>]
                         [--notification-transport <id> --notification-route <route>]
                         [--jitter-seconds <n>] [--description <text>] [--force]
homeboy schedule list
homeboy schedule show <id>
homeboy schedule run <id>
homeboy schedule enable|disable <id>
homeboy schedule remove <id>
homeboy schedule tick [--dry-run]
```

`schedule` declares homeboy commands that run on a cadence. Homeboy already owned durable jobs, typed notification transports, and a structured result envelope; what it lacked was a time trigger, so periodic work had to be bolted onto external cron or systemd timers that each re-solved notification wiring, overlap, and change detection ([issue #10073](https://github.com/Extra-Chill/homeboy/issues/10073)).

A schedule is stored in two parts:

- the **declaration** is reviewable configuration at `~/.config/homeboy/schedules/<id>.json`
- the **runtime record** — last status, last result fingerprint, in-flight marker — lives separately under the data directory, so the declaration stays diffable

## What a schedule runs

A schedule runs **either** a homeboy command or an external program — one or the other, never both.

```sh
# a homeboy command
homeboy schedule add fleet-drift --command 'fleet check prod' --every 1h

# an external program
homeboy schedule add e2e \
  --exec npm --exec-arg run --exec-arg test \
  --working-dir /srv/project \
  --every 24h
```

External programs are executed **directly, never through a shell**. Nothing is word-split, so an argument containing spaces stays one argument, and there is no quoting or injection surface. Shell operators (`|`, `&&`, `>`, `$`) in the program are refused at declaration time rather than failing later with a confusing "no such file" — to run a pipeline, put it in a script and schedule the script.

Arguments are passed through untouched, so an argument may legitimately contain those characters.

`--working-dir` matters for the common case: test runners and build tools usually only work from their project root.

## Cadence

`--every` takes a compact interval: `45s`, `30m`, `24h`, `1h30m`, `7d`. A bare number is rejected rather than guessed, because `--every 30` is ambiguous between seconds and minutes and guessing wrong runs a command sixty times more often than intended.

Cadence is measured from the previous run's **start**, so a slow run does not push every later run out by its own duration.

## Reporting

`--notify-on` decides when a completed run is worth interrupting a human for:

| Policy | Notifies |
|---|---|
| `change` (default) | when the result differs from the previous run, **and** on every failure |
| `failure` | only when the run fails |
| `always` | every run |

`change` is the default because the useful behavior for a periodic check is silence while healthy and a ping when something drifts — a notification should mean something needs attention.

Change detection depends on what ran:

- a **homeboy command** is fingerprinted from its result envelope, with volatile fields (timestamps, durations, run ids) removed — without that, every run would look like a change and `change` would be indistinguishable from `always`
- an **external program** has no envelope, so its combined stdout/stderr is fingerprinted together with its exit code. A probe whose output stops changing goes quiet; one whose output changes reports, even if the exit code is unchanged.

External output is captured up to 64 KiB and only the tail is kept, so a chatty program cannot grow the runtime state without bound. The notification carries a bounded tail of that output.

A repeated **identical failure** still notifies. An ongoing outage that goes quiet because it is "unchanged" is the worst available outcome.

Notifications are delivered through the same installed transports as `--notification-transport` / `--notification-route`, stored on the schedule so a triggered run does not need them passed again.

## Overlap and jitter

`--on-overlap skip` (default) declines to start a run while the previous one is still in flight, so a slow check cannot stack copies of itself. `--on-overlap allow` starts it anyway.

`--jitter-seconds` spreads runs across a window so that many schedules sharing a cadence do not all fire on the hour. The offset is derived from the schedule id, so a given schedule always lands at the same point in the window rather than walking across it on every restart.

## Running

`homeboy schedule run <id>` runs one schedule immediately, whether or not it is due. `homeboy schedule tick` runs everything currently due, and `--dry-run` reports what would run without running it.

Both exit non-zero when a scheduled run fails, so an external trigger can react without parsing the payload.

Scheduled commands execute as a subprocess of the homeboy binary and are read back through their `homeboy/command-result/v3` envelope. A scheduled command therefore cannot take its caller down with it.

Scheduling the `schedule` command itself is refused.

```sh
homeboy schedule add nightly-harvest \
  --command 'harvest production --check' \
  --every 24h \
  --notification-transport discord.run-completion \
  --notification-route 'discord:v1:channel:123456789012345678'

homeboy schedule add fleet-drift --command 'fleet check prod' --every 1h --jitter-seconds 300

homeboy schedule list
homeboy schedule tick --dry-run
```

## Triggering

A running daemon fires due schedules itself — `homeboy daemon start` is all that is required for declared schedules to run.

```sh
homeboy daemon start
homeboy schedule add nightly --command 'harvest production --check' --every 24h
```

The daemon polls every 30 seconds by default. That is the *polling* cadence, not a schedule's own cadence: it bounds how late a due schedule can fire, and polling more often than the shortest declared interval is harmless because a schedule that is not due is skipped. Override with `HOMEBOY_DAEMON_SCHEDULE_TICK_SECS`, or set it to `0` to turn daemon-driven scheduling off entirely.

Each due schedule runs on its own thread, so a slow scheduled command delays neither the poll loop nor daemon shutdown.

`homeboy schedule tick` remains available for operators who would rather drive scheduling from systemd, cron, or another supervisor — pair it with `HOMEBOY_DAEMON_SCHEDULE_TICK_SECS=0` so the two do not both fire.

```sh
# external trigger, with daemon-driven scheduling disabled
homeboy schedule tick
```

## Recovering an interrupted run

A schedule is marked in flight while it runs so an overlapping tick declines it. If the process is killed between that marker and the recorded result, the marker would otherwise block the schedule forever under `--on-overlap skip`.

The daemon clears markers older than six hours when it starts, in the same way it reconciles expired job reservations. A marker with no recorded start time cannot be aged and is cleared as well.
