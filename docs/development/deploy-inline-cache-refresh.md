# Archive deploy release isolation and inline cache refresh (#18365)

Archive installs (`deploy.archive_install`) publish a component in one of two
ways:

- **In-place exchange** (default). The staged tree is atomically exchanged
  with the live directory (`mv -T --exchange`, then `renameat2(RENAME_EXCHANGE)`
  through python3, then a backup two-rename fallback). The target path never
  goes missing, but its contents change underneath requests that already
  resolved it. One PHP request can include the old main file and then
  lazily autoload a new class file.
- **Release layout** (opt-in `release_layout`). Every deploy gets its own
  directory, and the target becomes a relative symlink that is flipped with a
  single `rename(2)`. A request that resolved the old release keeps reading
  the old release; new requests see the new one. Releases never mix.

Both flows run the extension's `after_switch_command` inline right after the
switch, before cleanup. The WordPress extension enables the release layout
for plugins and themes, gated by a PHP-FPM precondition.

## Layout

```text
<parent>/<slug>                                  -> .homeboy-releases/<slug>/<id>/<slug>
<parent>/.homeboy-releases/<slug>/<id>/<slug>/   active release
<parent>/.homeboy-releases/<slug>/<old-id>/<slug>/  retained previous release
<parent>/.homeboy-releases/<slug>/.deactivated-<old-id>  deactivation time (mtime)
```

- `<id>` is `YYYYmmddTHHMMSSZ-<artifact cksum hex>-<pid>`, sortable and unique.
- The release tree keeps the target basename (`<id>/<slug>`), so code that
  derives its identity from its own directory name (`basename(__DIR__)`) sees
  the same name as before.
- The release root must be one dot-prefixed path segment. WordPress skips
  dot-prefixed directories when it scans `wp-content/plugins` and
  `wp-content/themes`, so retained releases never show up as extra plugins
  or themes.
- The link is relative, so it survives moving or bind-mounting the parent.

## Install flow

1. Stage and validate the archive exactly as in-place installs do: adjacent
   `mktemp -d`, root-name check, extraction.
2. Run `release_layout.precondition_command` (when declared) in a subshell.
   Its stdout and stderr are logged as install evidence. Exit 0 selects the
   release layout. A nonzero exit selects the in-place flow and adds a
   `Release layout precondition failed (exit N); …` entry, with the output
   tail, to the component's `warnings`.
3. Move the staged tree to `<root>/<slug>/<id>/<slug>` and create a temp
   symlink `<parent>/.homeboy-link.<id>` beside the target.
4. Publish:
   - **Target is already a symlink:** `mv -fT tmp target` (or python3
     `os.replace`) flips the link in one `rename(2)`. The target is never
     removed and recreated. If the old link was not a managed release link,
     a warning is recorded and the old link destination is left alone.
   - **Target is a real directory (migration):** the temp symlink and the
     directory are swapped atomically with `mv -T --exchange` or
     `renameat2(RENAME_EXCHANGE)`. The old tree is then filed as release
     `<id>-migrated` and kept as the previous release. If no atomic exchange
     exists on the host, Homeboy warns and uses the in-place flow; it never
     opens a window where the target is missing.
   - **No target (first install):** the temp symlink is renamed into place.
   - **Anything else:** warning plus in-place fallback.
5. If publishing fails, the target is untouched (the previous release stays
   active), the new release directory is removed, and the install fails.
6. Run `after_switch_command` (best-effort, as before).
7. Garbage-collect old releases (best-effort, see below).

Whenever the in-place flow is used instead, any release directory prepared
for this deploy is discarded.

## Garbage collection

After a successful flip, Homeboy keeps:

- the active release (re-read from the symlink before every removal, so it
  is never deleted);
- the `keep_previous` (default 2) most recently deactivated releases;
- every release deactivated less than `keep_recent_minutes` (default 15)
  ago, so long-running requests that still execute an old release keep
  their files. Deactivation time is the mtime of the `.deactivated-<id>`
  marker, falling back to the release directory's mtime.

GC runs in a subshell and is wrapped in `|| true`. A failure to prune never
fails the deploy. The previous release is never removed by the deploy that
replaced it unless it falls outside both retention rules.

## Downstream behavior through the symlink

- Version probes, header verification (`required_header`), and post:deploy
  hooks read through `{{targetDir}}`, so they see the active release.
- The remote content manifest probe resolves a symlinked root
  (`root="$root/."`), so the manifest of a symlinked release equals the
  manifest of the same tree as a real directory.
- Permission fixes (`chmod` via `find`) and `chown -R`/`chgrp -R` resolve the
  symlinked root, so modes and ownership land on the release tree.
- The "target is a symlink" deploy notice stays quiet for managed release
  links (a relative link into a dot-prefixed directory).
- The WordPress ownership-warning hook uses `stat -L` so it reports the
  release tree's owner, not the link's.

## WordPress precondition

With `opcache.revalidate_path=0` (the PHP default), opcache keys cached
scripts by the unresolved path. After a symlink flip, FPM would keep serving
the old release's bytecode for `wp-content/plugins/<slug>/…` until the cache
is invalidated or expires. That reintroduces the stale-code problem the
layout is meant to fix. With `opcache.revalidate_path=1`, opcache resolves
the real path, so each release is cached under its own real directory and a
flip is picked up immediately.

`ini_get()` from the PHP CLI only sees the CLI's settings, so the WordPress
precondition talks FastCGI directly to each PHP-FPM socket. It is the same
client used for the inline opcache refresh, and it runs a tiny worker
script inside FPM that reports `opcache=disabled`, `revalidate_path=on`, or
`revalidate_path=off`. The precondition passes only when every socket
reports `opcache=disabled` or `revalidate_path=on`. It fails closed, and the
deploy falls back to the in-place flow, when:

- any socket reports `revalidate_path=off` or gives an unrecognized answer;
- a socket is unreachable;
- no writable socket is found (set `HOMEBOY_PHP_FPM_SOCKET` to a
  comma-separated list to override discovery);
- the php CLI is missing.

The FastCGI client has a single source,
`extensions/wordpress/scripts/deploy/fpm-fastcgi.php`.
`node extensions/wordpress/scripts/deploy/archive-runtime-commands.js --write`
regenerates both `after_switch_command` and `release_layout.precondition_command`
in `wordpress.json`, and the opcache smoke test fails if they drift.

### Operator requirement

To get release isolation on WordPress targets, enable path revalidation in
every PHP-FPM pool that serves the site, then reload FPM:

```ini
; e.g. /etc/php/8.4/fpm/conf.d/99-homeboy.ini or the pool's php_admin_value
opcache.revalidate_path=1
```

Until this is set, deploys keep working with the in-place flow and report the
precondition failure in `warnings`.

## Residual limits

- The guarantee covers the component's own files. A request that spans two
  components deployed in the same run can still see one old and one new
  component. Dependency ordering limits, but does not remove, that exposure.
- Old releases are kept for `keep_recent_minutes`. A request that runs longer
  than that, and also lazily loads a file afterwards, can still fail.
- PHP's realpath cache (`realpath_cache_ttl`, default 120 s) can resolve
  `wp-content/plugins/<slug>` to the previous release for up to that TTL per
  worker. Those requests run the previous release consistently; they do not
  mix releases.
- Hosts without `mv --exchange` (GNU coreutils < 9.5) and without python3
  with glibc ≥ 2.28 cannot migrate a real directory atomically. They stay on
  the in-place flow and record a warning.
- Turning the layout off again (or a later precondition failure) makes the
  in-place flow replace the managed symlink with a real directory. Retained
  releases under `.homeboy-releases` are left for an operator to remove.

## Verification

- `cargo test -p homeboy-deploy --lib`, including `release_layout::tests`:
  - concurrent readers across v1→v5, with no mixed main/class pair and no
    missing target, plus a deterministic negative showing in-place mixing;
  - migration (atomic, old tree retained, never missing), migration without
    an atomic exchange, precondition fallback with evidence, flip-failure
    preservation, unmanaged symlinks, GC count/age/active rules, GC failure
    being non-fatal, and the shipped WordPress policies failing closed;
  - the content manifest of a symlinked release equals that of a real
    directory (`content_manifest::tests`).
- `cargo test -p homeboy-extension-contract`: `release_layout` defaults and
  validation.
- `TMPDIR=/tmp node extensions/wordpress/tests/opcache-invalidation-hook-smoke.js`:
  - the fake FPM covers refresh, precondition On / Off / disabled / mixed /
    unrecognized / unreachable / no socket / no php, and manifest drift;
  - real `php-fpm` pools with `revalidate_path` 1 and 0 are also checked
    when a php-fpm binary is installed.
  - A short TMPDIR is needed because Unix socket paths are length-limited.
- `node extensions/wordpress/tests/wordpress-manifest-settings-smoke.js`

Refs Extra-Chill/homeboy#18365, #17922, #18153, #6867, #13569.
