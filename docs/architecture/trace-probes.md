# Trace Probes

Trace probes are passive observation helpers that run beside a trace workload and emit events into the existing trace timeline shape:

```json
{ "t_ms": 12, "source": "log.tail", "event": "log.match", "data": { "line": "..." } }
```

## Contract

- Probes never append to `HOMEBOY_TRACE_RESULTS_FILE` directly.
- Homeboy starts probes before invoking the trace runner and stops them after the runner exits.
- Probe events are collected internally, sorted by `t_ms`, and merged into the runner's final `timeline` before span processing.
- Runner-owned events and probe-owned events share the same envelope, so existing span definitions can target either source.

## Rig Workload Configuration

Probes are declared on detailed `trace_workloads` entries:

```jsonc
{
  "trace_workloads": {
    "nodejs": [
      {
        "path": "${package.root}/trace/create-site.trace.mjs",
        "trace_probes": [
          { "type": "log.tail", "path": "${components.app.path}/app.log", "grep": "invalid_grant" },
          { "type": "process.snapshot", "pattern": "opencode.*serve", "interval_ms": 250 },
          { "type": "file.watch", "path": "${components.app.path}/auth.json" },
          { "type": "port.snapshot", "port": 3000 },
          { "type": "http.poll", "url": "http://127.0.0.1:3000/health", "assert-status": 200 },
          { "type": "http.egress", "host": "api.example.com", "capture": "body" },
          { "type": "cmd.run", "command": "kimaki", "args": ["send", "--thread", "123", "--prompt", "test"] }
        ]
      }
    ]
  }
}
```

Probe values support the same `${components.<id>.path}` and `${package.root}` expansion as rig workload paths.

## v1 Probes

### `log.tail`

Inputs:

- `path`: log file to tail.
- `grep` or `match`: optional regex.

Events:

- `log.line`: emitted for each new line appended after the probe starts.
- `log.match`: emitted when the optional regex matches a line.

Data includes `path`, `line`, and `pattern` for match events.

### `process.snapshot`

Inputs:

- `pattern`: regex matched against process command lines.
- `interval_ms`: optional polling interval, default `1000`.

Events:

- `proc.list`: full matching process snapshot for each poll.
- `proc.spawn`: matching PID appeared since the previous snapshot.
- `proc.exit`: matching PID disappeared since the previous snapshot.

Data includes `pattern` and `processes` for list events, and `pid` plus `command` for delta events.

### `file.watch`

Inputs:

- `path`: file path to poll for existence, size, or modification-time changes.
- `interval_ms`: optional polling interval, default `250`.

Events:

- `fs.create`: path appeared after the probe started.
- `fs.write`: existing path changed size or modification time.
- `fs.delete`: path disappeared after the probe started.

Data includes `path`, `exists`, and `len` when the file exists.

The v1 implementation is polling-based and portable. It does not yet provide PID-of-writer attribution or rename pairing.

### `port.snapshot`

Inputs:

- `port`: one TCP port to monitor on `127.0.0.1`.
- `port-range`: inclusive `start-end` TCP port range.
- `interval_ms`: optional polling interval, default `500`.

Events:

- `net.listening`: matching ports that appear to be bound/listening.
- `net.bind`: matching port became bound since the previous snapshot.
- `net.unbind`: matching port became unbound since the previous snapshot.

Data includes `ports` for list events and `port` for delta events.

The v1 implementation uses a bind probe for portability. It detects whether a port is available, not the owning process or active connection flows.

### `http.poll`

Inputs:

- `url`: URL to fetch.
- `interval_ms`: optional polling interval, default `1000`.
- `assert-status`: optional expected HTTP status code.

Events:

- `http.response`: request completed with a response.
- `http.error`: request failed.

Data includes `url`, `status`, `latency_ms`, and `ok` when `assert-status` is configured.

### `http.egress`

Inputs:

- `host`: destination host filter. Exact hosts, `*`, and `*.example.com` suffix patterns are supported.
- `path`: optional path substring/glob-style filter.
- `capture`: `summary`, `headers`, or `body`. Defaults to `summary`.
- `max_body_bytes`: maximum inline request/response body bytes when `capture` is `body`. Defaults to `1048576`.
- `redact_headers`: optional header names to redact. Defaults include `Authorization`, `Proxy-Authorization`, `x-api-key`, `Cookie`, and `Set-Cookie`.
- `listen_port`: optional local proxy port. Defaults to an ephemeral port.

Events:

- `proxy.ready`: local proxy is listening; data includes `proxy_url` for `HTTP_PROXY` / `HTTPS_PROXY` injection.
- `http.request`: proxied HTTP request matched the filters.
- `http.response`: upstream HTTP response returned.
- `http.connect`: HTTPS CONNECT tunnel matched the filters.
- `http.error` / `proxy.error`: proxy or upstream failures.

V1 is a portable local proxy. It captures HTTP bodies and forwards HTTPS CONNECT tunnels while recording metadata; it does not install a CA or decrypt TLS body content. Operators who need HTTPS body capture can still use the same probe envelope once a MITM/CA mechanism is added.

### `cmd.run`

Inputs:

- `command`: executable to run.
- `args`: optional argument array.

Events:

- `cmd.start`: command was invoked.
- `cmd.stdout`: one event per stdout line collected from the command.
- `cmd.stderr`: one event per stderr line collected from the command.
- `cmd.exit`: command exited successfully or unsuccessfully.
- `cmd.error`: command could not be spawned.

Data includes `command` and `args` on start, `line` on output events, and `exit_code`, `success`, and `duration_ms` on exit.

## Deferred

This substrate intentionally does not implement fanotify, PID-of-writer attribution, systemd integration, port owner detection, active connection flow reporting, streaming command output before process exit, or rename pairing. Those can layer onto the same collection and merge contract later.
