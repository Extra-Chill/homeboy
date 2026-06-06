# `homeboy runtime`

Inspect Homeboy core-owned runtime assets used by extension runners.

## Helper Paths

Resolve the materialized path for a core runtime helper:

```bash
homeboy runtime helper path runner-prelude.sh
homeboy runtime helper path HOMEBOY_RUNTIME_COMMAND_CAPTURE
```

The command accepts either the helper filename or the injected `HOMEBOY_RUNTIME_*` environment variable name. Normal extension execution receives these paths automatically in the runner environment.
