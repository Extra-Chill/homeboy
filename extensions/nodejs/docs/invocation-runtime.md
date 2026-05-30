# Node.js Invocation Runtime Helper

Node workloads can import `resolveHomeboyInvocationRuntime()` to consume
Homeboy invocation isolation without parsing low-level environment variables in
each workload.

```js
const helper = process.env.HOMEBOY_NODEJS_INVOCATION_RUNTIME_HELPER;
const { resolveHomeboyInvocationRuntime } = await import(helper);

const runtime = resolveHomeboyInvocationRuntime({ namespace: 'site-build' });

await runtime.prepareDirs();

await spawnCommand({
  command: 'npm',
  args: ['test'],
  env: runtime.childEnv({ CI: '1' }),
});

runtime.assertPort(3000);
```

The helper normalizes these Homeboy core variables when present:

- `HOMEBOY_INVOCATION_ID`
- `HOMEBOY_INVOCATION_STATE_DIR`
- `HOMEBOY_INVOCATION_ARTIFACT_DIR`
- `HOMEBOY_INVOCATION_TMP_DIR`
- `HOMEBOY_INVOCATION_PORT_BASE`
- `HOMEBOY_INVOCATION_PORT_MAX`

When invocation isolation is active, state, artifact, and temporary directories
are scoped under the requested namespace. `runtime.env` and
`runtime.childEnv()` export those scoped paths for child commands, including
standard process isolation locations such as `HOME`, `TMPDIR`, and XDG state,
config, cache, and data directories.

`runtime.portRange` is `null` unless both port bounds are present. Missing one
bound, malformed bounds, out-of-range ports, and inverted ranges throw during
runtime resolution. `runtime.assertPort(port)` is a no-op when no range is
allocated and throws when an allocated range exists but the port is outside it.

The helper is product-agnostic. Workloads choose their own namespace and can
compose the returned directories into app-specific state, artifact, or process
manager paths.
