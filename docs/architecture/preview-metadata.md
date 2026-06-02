# Preview Metadata

Homeboy preserves generic preview metadata without owning public-access setup.
Commands, extensions, wrappers, or external integrations can emit preview facts
through environment variables, and Homeboy stores them on persisted run metadata
and renders them in report digests.

## Contract

Set `HOMEBOY_PREVIEW_JSON` to a JSON object. Recommended fields:

```json
{
  "schema": "homeboy/preview/v1",
  "provider": "dummy",
  "local_url": "http://127.0.0.1:8080",
  "public_url": "https://preview.example.test/run-1",
  "hold_seconds": 600,
  "expires_at": "2026-06-01T22:00:00Z",
  "status": "running",
  "process_id": "pid-123",
  "runtime_id": "runtime-abc",
  "cleanup_status": "pending"
}
```

`HOMEBOY_PREVIEW_PUBLIC_URL` may be set by a caller or integration that creates
public access. When present, Homeboy copies it into `public_url` if the preview
object does not already contain that field.

Homeboy does not create tunnels, publish previews, or know about product-specific
runtimes. Public access is supplied by the caller/integration.

## Persistence

When a command records an observation run, Homeboy stores the object at
`metadata.preview`. Lab offload forwards `HOMEBOY_PREVIEW_JSON` and
`HOMEBOY_PREVIEW_PUBLIC_URL` to the runner so offloaded commands preserve the
same caller-supplied preview facts.

## Reporting

`homeboy report performance-digest` renders scalar preview fields in a
`Preview` section, including local URL, public URL, hold/expiry, lifecycle
status, runtime/process ID, and cleanup status when available.
