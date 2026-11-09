# Scratch

Owned scratch space for verification, replay, proof and recovery work done
outside a Cook. Put those bytes here instead of an ad hoc path such as
`~/Developer/artifacts` or `~/.nr`. Nothing can see or reclaim an ad hoc path.

```sh
dir=$(homeboy scratch allocate --owner '#17240' --purpose 'paired proof' --ttl 2d | jq -r .data.allocation.path)
homeboy scratch list
homeboy scratch pin <id> --reason 'evidence linked from #17240'
homeboy scratch unpin <id>
homeboy scratch release <id>
```

- `allocate` creates a directory under the runtime-temp root. Before the path is
  returned, its owner record holds the lease: `owner` (the issue, run or
  session), `purpose`, and `expires_at` (now plus `--ttl`, which defaults to
  `3d`).
- `list` reports every allocation on this host, with its size and whether
  cleanup may remove it now.
- `pin` keeps an allocation past its TTL while a tracker or run still references
  it. `unpin` returns it to its TTL.
- `release` deletes an allocation immediately, whether or not it is pinned.

Allocations are runtime-temp owners with producer `scratch`.
`homeboy cleanup retained-storage` reports them under `runtime_tmp`.
Runtime-temp cleanup, including `homeboy cleanup automatic-retention`, keeps an
allocation while it is unexpired or pinned, and removes it once its TTL has
passed unpinned.

Scratch is local to the host that runs the command. On a runner, run it there:
`homeboy runner exec <runner> -- homeboy scratch allocate ...`.
