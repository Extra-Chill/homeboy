# Data Machine Agent CI Driver fixture

Generic WP Codebox plugin scaffold consumed by Data Machine agent CI runs.

## Why

Data Machine agent CI workflows need a stable plugin path inside WP Codebox
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
    "/host/path/homeboy-extensions/wordpress/tests/fixtures/datamachine-agent-ci-driver/datamachine-agent-ci-driver.php:/wordpress/wp-content/plugins/datamachine-agent-ci-driver/datamachine-agent-ci-driver.php:readonly"
  ],
  "transcript_dir": "/wordpress/wp-content/plugins/datamachine-agent-ci-driver/artifacts/<agent-slug>",
  "wp_codebox_workloads": [
    {
      "id": "<scenario-id>",
      "label": "<human label>",
      "run": [
        { "type": "php", "file": "/wordpress/wp-content/plugins/datamachine-agent-ci-driver/<workload>.php" }
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
