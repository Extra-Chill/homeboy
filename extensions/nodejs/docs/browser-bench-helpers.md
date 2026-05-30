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

Browser profile, artifact, and bottleneck result rows use the shared
product-neutral shapes documented in `../../docs/browser-result-shapes.md`.

The helper is substrate-agnostic. Assertions describe generic browser/page
facts such as status, selectors, text, title, and artifact presence. Workloads
can provide `sanitizeArtifacts()` or `sanitizeRawResult()` hooks to redact or
normalize generated artifacts before returning results.
