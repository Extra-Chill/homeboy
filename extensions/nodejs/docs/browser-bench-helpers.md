# Node.js Browser Bench Helpers

Browser benchmark workloads can import the helper path exported by
`HOMEBOY_NODEJS_BROWSER_BENCH_HELPER`.

```js
const { runBrowserPageScenario } = await import(process.env.HOMEBOY_NODEJS_BROWSER_BENCH_HELPER);

export default async function () {
  return runBrowserPageScenario({
    id: 'homepage',
    target: 'https://example.test/',
    assertions: [
      { type: 'status', expected: 200 },
      { type: 'selector', selector: 'main' },
      { type: 'artifact', key: 'trace', kind: 'playwright-trace' },
    ],
    action: async ({ page, mark }) => {
      await page.getByRole('heading').first().waitFor();
      await mark('heading_visible');
    },
  });
}
```

`runBrowserPageScenario()` wraps the lower-level `runBrowserBench()` lifecycle
without changing that API. It opens the target page, runs workload actions,
checks page and artifact assertions, writes a stable raw result artifact, and
returns `{ metrics, artifacts }` for the Homeboy bench runner.

## Public Artifact Links

Lab review runs can set `HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL` to the public
base where Homeboy publishes the run artifact directory. The generic Node.js
bench runner uses that base to add `url` fields to relative artifact paths and
absolute paths under `HOMEBOY_BENCH_ARTIFACTS_DIR`. Existing `http`/`https`
artifact paths are preserved as their public URL.

Workloads can attach opaque viewer metadata to any artifact. The runner keeps
that metadata as JSON without interpreting product-specific fields:

```js
return {
  artifacts: {
    evidence: {
      path: 'evidence.json',
      kind: 'json',
      label: 'Evidence',
    },
    replay: {
      path: 'viewer-input.json',
      kind: 'json',
      label: 'Viewer input',
      viewer: {
        kind: 'example-viewer',
        url: 'https://viewer.example/?artifact=viewer-input.json',
        metadata: { owned_by: 'consumer' },
      },
    },
  },
};
```

Consumers should publish stable review links for at least `report.md`,
`evidence.json`, and any viewer URLs they expose. Viewer URL construction stays
consumer-owned; this extension only preserves the metadata and resolves artifact
URLs from the public base.

Browser profile, artifact, and bottleneck result rows use the shared
product-neutral shapes documented in `../../docs/browser-result-shapes.md`.

The helper is substrate-agnostic. Assertions describe generic browser/page
facts such as status, selectors, text, title, and artifact presence. Workloads
can provide `sanitizeArtifacts()` or `sanitizeRawResult()` hooks to redact or
normalize generated artifacts before returning results.
