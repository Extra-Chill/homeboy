# WP Codebox Runtime Integration

This package contains shared adapters for direct WP Codebox operations used by
the WordPress extension. It is not a Homeboy agent-task provider.

The supported surface covers runtime contract discovery, version selection,
readiness, runtime profiles, artifacts, and result normalization. WordPress
sandbox, browser, preview, fuzz, WP-CLI, and artifact workflows invoke WP
Codebox's public runtime contracts directly.

## CLI Resolution

CLI resolution uses exact configured pins in this order:
`HOMEBOY_WP_CODEBOX_BIN`, `WP_CODEBOX_BIN`,
`HOMEBOY_SETTINGS_WP_CODEBOX_BIN`, and configured runtime settings. A configured
pin must name a usable executable. With no configured pin, a present managed
cache is selected, followed by `PATH`.

Managed setup records the source Git revision and SHA-256 of the built CLI in
`.homeboy-runtime-identity.json`. Readiness verifies both values. The WordPress
extension manifest owns the minimum supported WP Codebox version.

## Installed Layout

This package ships inside the WordPress extension at `wp-codebox-runtime/`.
Checkout and installed copies use the same in-extension paths.
