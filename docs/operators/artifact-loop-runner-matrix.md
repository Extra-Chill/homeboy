# Artifact Loop For Runner And Matrix Workflows

Use this loop when a runner, matrix job, or CI workflow produces output that a
reviewer should inspect later:

1. Run the workload on the intended runner or CI worker.
2. Write structured output and reviewer assets under a declared output directory.
3. Promote or attach that directory to the Homeboy run when the command supports it.
4. Verify the result through `homeboy runs show`, `homeboy runs artifacts`, and `homeboy runs evidence`.
5. Share reviewer-visible URLs from `runs evidence`, not machine-local paths.

The key distinction is stdout versus evidence. Stdout is useful operator context,
but it is not durable review evidence unless it is captured as an artifact or a
persisted run record. Prefer small JSON summaries plus static HTML, screenshots,
logs, or archives that can be fetched through Homeboy artifact commands.

## Artifact Mechanisms

Use the narrowest mechanism that matches the workflow:

| Mechanism | Use For | Review Shape |
|---|---|---|
| `--output <path>` | Command JSON handoff for one Homeboy command. | A local file until CI uploads or a run records it. |
| `HOMEBOY_ARTIFACT_ROOT` / `--artifact-root <dir>` | Controller-side persisted run artifacts. | `homeboy://run/...` refs plus `runs artifact get` fetch commands. |
| Runner daemon artifacts | Files produced by daemon-backed `runner exec` jobs. | `runner-artifact://...` refs and mirrored run artifacts when available. |
| Rig artifacts | Environment-owned files tied to a rig lifecycle or workload. | Best when the rig owns the service state and artifact paths. |
| `homeboy runs export` | Portable metadata bundle for moving observation records. | Metadata-only v1 bundle; artifact bytes are not copied. |
| Public artifact base URL | Reviewer-visible links for already-promoted file artifacts. | Public HTTP(S) links after validation. |

Avoid embedding private paths, local hostnames, or temporary runner directories in
PR comments. Let `runs evidence` translate recorded artifacts into fetch commands
or public links.

## Generic Runner Command

For ad hoc runner work, create a known output directory inside the runner
workspace and pass a stable run id:

```sh
run_id="review-static-html-$(date -u +%Y%m%dT%H%M%SZ)"

homeboy runner exec lab-runner \
  --cwd /workspace/example-component \
  --run-id "$run_id" \
  -- \
  sh -lc 'mkdir -p artifacts/review && npm run build-report -- --out artifacts/review'

homeboy runs show "$run_id"
homeboy runs artifacts "$run_id"
homeboy runs evidence "$run_id"
```

If `runs evidence` reports success with zero artifacts, the workload is not yet
reviewable. Keep the run id and output directory, then use the artifact
promotion or attach command once available. Proposed future shapes are:

```sh
# Proposed/upcoming: attach a runner-side directory to an existing run.
homeboy runner artifact promote lab-runner "$run_id" \
  --path artifacts/review \
  --kind static_html \
  --entry index.html

# Proposed/upcoming: attach a local directory to a persisted run.
homeboy runs artifact attach "$run_id" artifacts/review \
  --kind static_html \
  --entry index.html
```

Until those commands exist, prefer workflows that already register artifacts
through the runner daemon, a command-specific artifact contract, CI artifact
upload, or `runs import --from-gh-actions`.

## Static HTML Example

Static HTML reports work well when paired with a small JSON manifest:

```text
artifacts/review/
  index.html
  manifest.json
  screenshots/
    homepage.png
  data/
    matrix-summary.json
```

Example manifest:

```json
{
  "schema": "example/review-artifacts/v1",
  "entry": "index.html",
  "summary": "Static review report for the runner workload.",
  "artifacts": [
    { "path": "index.html", "kind": "static_html" },
    { "path": "screenshots/homepage.png", "kind": "screenshot" },
    { "path": "data/matrix-summary.json", "kind": "matrix_summary" }
  ]
}
```

The HTML should use relative links so it works whether Homeboy fetches it into a
local directory, CI uploads it as an archive, or a public artifact base URL serves
it from a mirrored artifact root.

## Matrix Example

Matrix cells should produce one artifact directory per cell plus an aggregate
summary. Keep axis metadata in JSON so `runs evidence` can later group cells
without understanding the project domain:

```sh
run_id="matrix-review-20260625"

for runtime in runtime-a runtime-b; do
  for browser in chromium firefox; do
    cell="runtime=${runtime},browser=${browser}"
    out="artifacts/matrix/${runtime}/${browser}"

    homeboy runner exec lab-runner \
      --cwd /workspace/example-component \
      --run-id "${run_id}-${runtime}-${browser}" \
      -- \
      sh -lc "mkdir -p ${out} && ./scripts/run-cell --runtime ${runtime} --browser ${browser} --out ${out}"
  done
done

./scripts/render-matrix-summary \
  --input artifacts/matrix \
  --output artifacts/matrix/index.html \
  --json artifacts/matrix/matrix-summary.json
```

Proposed/upcoming aggregate attach shape:

```sh
homeboy runs artifact attach "$run_id" artifacts/matrix \
  --kind matrix_static_html \
  --entry index.html
homeboy runs evidence "$run_id"
```

For rig matrix work, use rig artifacts when the rig owns setup, service state,
or workload paths. Use generic runner artifacts when the command is an ad hoc
worker command and the output directory is enough to understand the result.

## Public Links

Set `HOMEBOY_PUBLIC_ARTIFACT_BASE_URL` on the runner only when promoted artifact
files are mirrored to a stable HTTP(S) origin. With that variable configured,
Homeboy can derive public links for fetchable run artifacts and `runs evidence`
can expose reviewer-visible URLs after validation.

If the variable is absent, evidence remains valid but non-public: reviewers use
`homeboy runs artifact get <run-id> <artifact-id> -o <path>` or CI-provided
artifact downloads.

## Success Checklist

- `homeboy runs show <run-id>` lists the run and does not rely on stdout alone.
- `homeboy runs artifacts <run-id>` lists at least one file, URL, or remote file artifact.
- `homeboy runs evidence <run-id>` shows reviewer-visible public links or explicit fetch commands.
- Matrix output includes axis metadata and one aggregate summary artifact.
- Shared docs and PR comments use public URLs or Homeboy artifact refs, not local paths.
