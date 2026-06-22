# homeboy observe

Passively observe a running system and persist evidence in the observation store as run kind `observe`.

## Usage

```sh
homeboy observe <component> --duration 30s --tail-log /path/to/app.log --grep 'invalid_grant'
homeboy observe <component> --duration 5m --watch-process 'opencode-ai/bin/.*serve'
homeboy observe <component> --duration 5m --watch-process 'node .*serve' --watch-process-interval 1s
homeboy observe <component> --duration 5m --tail-log /path/to/app.log --watch-process 'node .*serve'
homeboy observe <component> --duration 30s --probe '{"type":"http.poll","url":"http://127.0.0.1:3000/health","assert-status":200}'
homeboy observe <component> --duration 60s --probe '{"type":"http.egress","host":"api.example.com","capture":"body"}'
```

## V1 Scope

`observe` is a passive producer. It samples or tails a system that is already
running and persists timeline evidence. It does not drive a scenario, assert
behavior, attach to privileged OS probes, or own target lifecycle.

Use `trace` when Homeboy should execute a declared scenario, collect
runner-owned artifacts, and evaluate assertions. Use `observe` when the target is
already live and the goal is to capture supporting evidence before or during a
manual investigation.

V1 supports:

- `--duration <duration>` using `ms`, `s`, `m`, or `h`
- `--tail-log <path>` for one or more log files
- `--grep <regex>` to filter tailed log lines
- `--watch-process <regex>` for process command-line polling
- `--watch-process-interval <duration>` for process polling cadence; defaults to `1s`
- `--probe <json>` for repeatable portable trace probe configs (`log.tail`, `process.snapshot`, `file.watch`, `port.snapshot`, `http.poll`, `http.egress`, and `cmd.run`)

`http.egress` starts a local proxy and emits a `http.egress proxy.ready` event with `proxy_url`. Point a target process at that URL with `HTTP_PROXY` / `HTTPS_PROXY` to capture proxied traffic. V1 captures HTTP request/response bodies and HTTPS `CONNECT` metadata; it does not decrypt TLS tunnels.

## Output

The command writes a trace-compatible JSON envelope to a run directory and records it as an artifact on an observation-store run:

```json
{
  "component_id": "wp-coding-agents",
  "scenario_id": "observe",
  "status": "pass",
  "summary": "Passive observation timeline",
  "timeline": [
    { "t_ms": 0, "source": "observe", "event": "started" },
    { "t_ms": 251, "source": "log", "event": "line", "data": { "path": "/root/.kimaki/kimaki.log", "line": "HTTP 400 invalid_grant" } },
    { "t_ms": 0, "source": "process", "event": "matched", "data": { "pattern": "opencode-ai/bin/.*serve", "pid": "1234", "command": "node opencode-ai/bin/opencode serve" } },
    { "t_ms": 1002, "source": "process", "event": "spawn", "data": { "pattern": "opencode-ai/bin/.*serve", "pid": "1235", "ppid": "1234", "command": "node opencode-ai/bin/opencode serve" } },
    { "t_ms": 2004, "source": "process", "event": "exit", "data": { "pattern": "opencode-ai/bin/.*serve", "pid": "1235", "was_command": "node opencode-ai/bin/opencode serve" } }
  ],
  "assertions": [],
  "artifacts": []
}
```

Inspect persisted evidence with:

```sh
homeboy runs list --kind observe
homeboy runs show <run-id>
homeboy runs artifacts <run-id>
```

Use `observe` to gather live evidence before encoding a deterministic `homeboy trace` workload with assertions.
