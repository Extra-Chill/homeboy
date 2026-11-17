# Inline archive runtime cache refresh (#18365)

## Design choice / PR body

This is the explicitly permitted **partial fix**, not request-level release isolation.
WordPress plugin and theme archive policies now run their existing best-effort
FastCGI opcache invalidation inside the install command, immediately after
publication and before old-tree cleanup. There is no intervening permissions,
verification, SSH round trip, or post:deploy hook dispatch. Ownership warnings
remain post:deploy. The generic deploy layer only knows an optional
`after_switch_command`; PHP/FPM logic remains in the WordPress extension.

The atomic exchange, fallback rollback, ownership/permissions processing,
verification and deploy evidence paths remain in place. Refresh stdout/stderr
are part of install evidence; refresh failure does not undo a successful swap.
First installs also refresh. No changelog or version files are changed.

## Why not release symlinks in this patch?

The investigation in #6867 confirms that WordPress normally supports symlinked
plugins: `wp_register_plugin_realpath()` registers the logical-to-real directory
mapping, `plugin_basename()` maps real paths back to the plugin slug, and
`plugins_url()` uses that basename for public URLs. It also documents a warm
worker realpath-cache failure during layout transitions, yielding workspace
paths in asset URLs. Symlinks are therefore not rejected as fundamentally
incompatible. However, safely migrating a live directory, pinning both mapping
and include resolution across worker caches, and retaining old trees for the
actual maximum request lifetime need a larger, runtime-aware implementation.
This patch deliberately does not claim those guarantees or add premature GC.

## Remaining risk

Publication and FPM invalidation are still separate operations, even in one
shell script. A request can execute between them, and already-running requests
can lazy-load new files through the unchanged path. Missing/unwritable FPM
sockets and other refresh failures remain best-effort skips. Thus the goal of
never mixing releases remains open; this patch narrows exposure only. A
concurrent-reader test asserting no mixed pair would be false for this design.
The existing concurrent never-missing-target regression remains, but is not
proof of request-level consistency.

## Verification

- `cargo test -p homeboy-deploy --lib`: archive replacement, fallback rollback,
  permissions/evidence tests, concurrent target-existence regression, plus new
  inline ordering/heredoc/nonfatal-failure and WordPress rendering tests.
- `TMPDIR=/tmp node extensions/wordpress/tests/opcache-invalidation-hook-smoke.js`:
  fake FPM protocol, generated worker, cleanup and best-effort skip paths.
  A short TMPDIR is needed because Unix socket names have a small length limit.
- `node extensions/wordpress/tests/wordpress-manifest-settings-smoke.js`
- `cargo fmt --all --check`, `git diff --check`

Refs Extra-Chill/homeboy#18365, #17922, #18153, #6867, #13569.
