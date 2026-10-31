# Node.js Browser Bench Helpers

Browser benchmark workloads can import the helper path exported by
`HOMEBOY_NODEJS_BROWSER_BENCH_HELPER`.

## Extension-Owned Playwright

The Node.js extension owns a pinned Playwright package and its Chromium setup.
Install or inspect it through the extension actions:

```sh
homeboy extension action nodejs browser.playwright.setup
homeboy extension action nodejs browser.playwright.status
```

The utility stores JavaScript dependencies under the runner cache and lets
Playwright use its normal runner-wide browser cache. It never installs into a
project workspace. Browser modules can run without project `node_modules`:

```sh
homeboy extension action nodejs browser.playwright.execute --data '[{"module":"/absolute/scripts/capture.mjs","args":[]}]'
```

The module can use `import { chromium } from 'playwright'` or CommonJS
`require('playwright')`. This explicit utility always resolves the pinned
extension runtime so its package and Chromium revisions remain paired.
Project-local Playwright workflows outside this utility are unchanged. Pass
module arguments after the module path when invoking the wrapper script
directly: `playwright.sh execute scripts/capture.mjs --target https://example.test/`.

Node.js `>=20.6.0` is required because the wrapper uses `node:module`
`register()`. The utility verifies that its runtime directory is same-UID,
non-symlinked, and private before use. This is a same-UID runner trust boundary;
it does not isolate code that can already run as that user.

For Lab or CI integration coverage, run the real browser check after setup:

```sh
homeboy extension action nodejs browser.playwright.setup
HOMEBOY_RUN_PLAYWRIGHT_INTEGRATION=1 bash nodejs/scripts/bench/nodejs-browser-helper-smoke.sh
```

```js
const { buildBrowserBenchResult, runBrowserPageScenario } = await import(process.env.HOMEBOY_NODEJS_BROWSER_BENCH_HELPER);

export default async function () {
  const browserResult = await runBrowserPageScenario({
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

  return buildBrowserBenchResult({
    browserResult,
    metrics: { success_rate: 1 },
  });
}
```

`runBrowserPageScenario()` wraps the lower-level `runBrowserBench()` lifecycle
without changing that API. It opens the target page, runs workload actions,
checks page and artifact assertions, writes a stable raw result artifact, and
returns `{ metrics, artifacts }` for the Homeboy bench runner.

`buildBrowserBenchResult()` composes those browser metrics and artifacts with
workload-owned fields while preserving the Homeboy core `BenchScenario` result
shape.

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

## Deferred-Initialization Evidence

Browser workloads can import the same helper path to prove that optional browser
work stays idle until a feature is needed:

```js
const {
  deferredInitBrowserMarkerScript,
  deferredInitBrowserMarkers,
  summarizeDeferredInitBrowserEvidence,
} = await import(process.env.HOMEBOY_NODEJS_BROWSER_BENCH_HELPER);

const featureId = 'lazy-widget';
const markers = deferredInitBrowserMarkers(featureId);

await page.addInitScript(deferredInitBrowserMarkerScript(featureId));
await page.goto(target);
await page.evaluate((featureId) => {
  const deferred = window.__homeboyDeferredInit[featureId];
  deferred.mark(deferred.markers.featureNotNeededReady, { state: 'idle' });
}, featureId);

await page.getByRole('button', { name: 'Show widget' }).click();
await page.evaluate((featureId) => {
  const deferred = window.__homeboyDeferredInit[featureId];
  deferred.mark(deferred.markers.featureNeededTrigger, { trigger: 'button-click' });
}, featureId);

await page.waitForSelector('[data-widget-ready="true"]');
const markerEvents = await page.evaluate((featureId) => {
  const deferred = window.__homeboyDeferredInit[featureId];
  deferred.mark(deferred.markers.featureNeededReady);
  deferred.mark(deferred.markers.featureNeededSuccess);
  return deferred.events;
}, featureId);

const summary = summarizeDeferredInitBrowserEvidence({
  featureId,
  markerEvents,
  networkEntries: browserProfile.network,
  featureRequestMatchers: ['lazy-widget'],
  thirdPartyRequestMatchers: [/third-party\.example/],
  maxEarlyFeatureRequests: 0,
  maxEarlyThirdPartyRequests: 0,
  minPostTriggerFeatureRequests: 1,
});
```

The helper is product-neutral. The workload owns selectors, interaction steps,
and request matchers; the helper only normalizes marker names, metrics,
assertions, URL samples, and phase labels.

Default phases:

```json
{
  "FEATURE_NOT_NEEDED": "feature-not-needed",
  "FEATURE_NEEDED": "feature-needed"
}
```

Default markers use `deferred_init.<featureId>.*`:

```json
{
  "featureNotNeededStart": "deferred_init.lazy-widget.feature_not_needed.start",
  "featureNotNeededReady": "deferred_init.lazy-widget.feature_not_needed.ready",
  "featureNeededTrigger": "deferred_init.lazy-widget.feature_needed.trigger",
  "featureNeededReady": "deferred_init.lazy-widget.feature_needed.ready",
  "featureNeededSuccess": "deferred_init.lazy-widget.feature_needed.success"
}
```

Summary output shape:

```json
{
  "feature_id": "lazy-widget",
  "phases": {
    "FEATURE_NOT_NEEDED": "feature-not-needed",
    "FEATURE_NEEDED": "feature-needed"
  },
  "metrics": {
    "lazy_widget_deferred_init_feature_not_needed_ready_ms": 125,
    "lazy_widget_deferred_init_feature_needed_trigger_ms": 500,
    "lazy_widget_deferred_init_feature_needed_ready_ms": 725,
    "lazy_widget_deferred_init_feature_needed_success_ms": 750,
    "lazy_widget_deferred_init_request_timing_available": true,
    "lazy_widget_deferred_init_feature_request_count_before_trigger": 0,
    "lazy_widget_deferred_init_feature_request_count_after_trigger": 2,
    "lazy_widget_deferred_init_no_early_feature_init": true,
    "lazy_widget_deferred_init_post_trigger_feature_requests": true,
    "lazy_widget_deferred_init_post_trigger_success": true
  },
  "assertions": [
    {
      "id": "lazy-widget-no-early-feature-init",
      "status": "pass",
      "message": "Observed 0 feature request(s) before trigger; expected <= 0."
    }
  ],
  "metadata": {
    "early_feature_urls_sample": [],
    "post_trigger_feature_urls_sample": ["https://cdn.example.test/lazy-widget.js"],
    "early_third_party_urls_sample": [],
    "post_trigger_third_party_urls_sample": []
  }
}
```

Consumers can attach `summary.metrics` to benchmark metrics and record
`summary.assertions` in their evidence traces. Full network logs stay in the
workload artifact; the helper returns bounded URL samples for review summaries.
