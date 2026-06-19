# Runtime Agent Driver Fixture

Generic WP Codebox plugin scaffold consumed by runtime agent runs.

## Why

Runtime agent workflows need a stable plugin path inside WP Codebox
to host workloads, bundle copies, and transcript artifacts. Without a fixture
each consumer ships its own near-identical `<repo>-ci-driver.php`.

This fixture removes that duplication. Consumers mount it via the agent
runner's `wp_codebox_mounts` mechanism and reference its plugin path
in their workload `run` entries and `transcript_dir` config.

## How consumers use it

In the agent runner config JSON:

```json
{
  "wp_codebox_mounts": [
    "/host/path/homeboy-extensions/wordpress/tests/fixtures/runtime-agent-driver/runtime-agent-driver.php:/wordpress/wp-content/plugins/runtime-agent-driver/runtime-agent-driver.php:readonly"
  ],
  "transcript_dir": "/wordpress/wp-content/plugins/runtime-agent-driver/artifacts/<agent-slug>",
  "wp_codebox_workloads": [
    {
      "id": "<scenario-id>",
      "label": "<human label>",
      "run": [
        { "type": "php", "file": "/wordpress/wp-content/plugins/runtime-agent-driver/<workload>.php" }
      ]
    }
  ]
}
```

The reusable workflow mounts this fixture by default, so no new runner behavior
is required to use it.

## What it does NOT do

- No abilities, hooks, or runtime registrations.
- No data persistence.
- No CLI commands.

It is intentionally a no-op plugin header, used purely as a path anchor.
