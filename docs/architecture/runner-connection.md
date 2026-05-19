# Runner Connection Bootstrap

`homeboy runner connect <runner-id>` uses the first-class runner registry added
for issue #2526.

## Registry Contract

Runner configs are JSON files at `~/.config/homeboy/runners/<id>.json` with the
initial #2526 shape:

```json
{
  "kind": "ssh",
  "server_id": "lab-box",
  "workspace_root": "/srv/homeboy-lab",
  "homeboy_path": "homeboy",
  "daemon": true
}
```

Only `kind: "ssh"`, `server_id`, and optional `homeboy_path` are used by the
Wave 1 connection commands. Registry CRUD owns creation, validation, and future
execution metadata such as `workspace_root`, `concurrency_limit`, `env`, and
`resources`.

## Connection Shape

The remote daemon is started with `homeboy daemon start --addr 127.0.0.1:0` and
the reported address is rejected unless it is loopback. The local client reaches
the daemon through an SSH `-L 127.0.0.1:<local>:127.0.0.1:<remote>` tunnel.

Session metadata is stored at `~/.config/homeboy/runner-sessions/<id>.json` so
`status` and `disconnect` can inspect or close the local tunnel later.
