# Static Site Fixture Matrix

The static-site fixture matrix runner turns an arbitrary directory of static HTML fixtures into WP Codebox-ready SSI validation artifacts.

```bash
npm run static-site-fixture-matrix -- --fixture-root /path/to/import-fixtures --output-directory ./artifacts/ssi-fixture-matrix
```

Add `--run` to execute the generated WP Codebox recipe. Without `--run`, the command writes the matrix, per-fixture website artifact JSON, finding packets, and recipe for review. With `--run`, the command parses WP Codebox step output and any per-fixture JSON artifacts, then rewrites `static-site-fixture-matrix-result.json`, `summary.json`, and `finding-packets.json` with per-fixture diagnostics.

Outputs:

- `matrix.json` lists discovered fixture directories with `index.html`.
- `<fixture>/artifact.json` is the SSI website artifact passed to `wp static-site-importer validate-in-codebox`.
- `wp-codebox-static-site-fixture-matrix-recipe.json` is the WP Codebox recipe.
- `static-site-fixture-matrix-result.json`, `summary.json`, and `finding-packets.json` normalize diagnostics for fanout.

Collected fixture results preserve partial output when WP Codebox or a fixture command fails. Diagnostics are grouped into `runtime_target_gap`, `invalid_block_content`, `dropped_images`, `broken_svg`, `button_style_loss`, or the generic `static_site_import_quality` fallback.

The installed Homeboy rig id is `static-site-fixture-matrix`; `homeboy rig check static-site-fixture-matrix` verifies the runner and smoke test.
