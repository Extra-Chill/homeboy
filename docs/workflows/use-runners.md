# Use Runners

Runners let Homeboy route hot or remote-capable work away from the controller machine while preserving the same command contract and evidence shape.

## Check Runner Health

```bash
homeboy runner list
homeboy runner status <runner-id>
homeboy runner doctor <runner-id>
```

## Route A Gate Through A Runner

Let Homeboy choose the configured runner when available:

```bash
homeboy review --changed-since origin/main
```

Or select one explicitly:

```bash
homeboy --runner <runner-id> review --changed-since origin/main
```

## Execute From A Runner-Side Checkout

```bash
homeboy runner exec <runner-id> \
  --cwd /srv/homeboy/checkouts/my-component \
  -- homeboy review my-component --changed-since origin/main
```

## Reference

- [runner command](../commands/runner.md)
- [Release-gate proof path](../operations/release-gate-proof-path.md)
- [Controller to runner reverse-runner setup](../operations/controller-runner-reverse-runner.md)
