# Applying reviewed edge config from an agent host

An agent with a shell on a production host can usually diagnose a server-level
defect and write the fix long before anyone can apply it. Applying it is the
part that stalls, because the only honest options have been "no root" or "root",
and the second one is not an option.

`scripts/homeboy-edge-apply` is the third: the agent can apply config that a
human already merged, and cannot apply anything else.

## The model

Install the wrapper at `/usr/local/sbin/homeboy-edge-apply` (root:root, 0755)
and grant the agent account exactly one line:

```
agent ALL=(root) NOPASSWD: /usr/local/sbin/homeboy-edge-apply *
```

The caller passes a **target name**. It never passes content, a path, or a
repository. Everything consequential lives in root-owned config at
`/etc/homeboy-edge.d/<target>.conf`, which the agent cannot write.

```
REPO_URL=https://github.com/example/infrastructure.git
TRUST_REF=refs/heads/main
SOURCE_PATH=deploy/nginx/sites-enabled/example
DEST_PATH=/etc/nginx/sites-enabled/example
DEST_MODE=0644
VALIDATE_CMD=/usr/sbin/nginx -t
RELOAD_CMD=/usr/bin/systemctl reload nginx
PROBE=https://example.com/ 200
PROBE=https://example.com/health 200 example.com:443:127.0.0.1
```

Then:

```
sudo /usr/local/sbin/homeboy-edge-apply example-nginx
sudo /usr/local/sbin/homeboy-edge-apply example-nginx <full-40-char-sha>
```

## Why it fetches for itself

The obvious implementation — copy the file from a checkout already on the host —
is a root shell with extra steps. Agent hosts keep working checkouts owned by
the agent account. If the wrapper reads a checkout the caller can write, the
caller writes whatever it wants first and the allowlist constrains only where
the bytes land, not what they are. For nginx specifically, arbitrary config is
arbitrary root: `root /` plus `alias` serves `/etc/shadow`, and `proxy_pass`
reaches any service bound to loopback.

So the wrapper does its own `git fetch` into a mirror under
`/var/lib/homeboy-edge/mirrors/` (0700, root) and reads content with
`git cat-file` directly from the object store. It never reads a working tree.

The trust anchor is `TRUST_REF`, default `refs/heads/main`. An explicitly
requested commit must be an ancestor of it, so the only applicable content is
content that was merged — which on any normal repository means a human approved
it in a pull request. The review that already exists becomes the authorization
boundary, instead of a second, weaker one being invented.

Remotes are restricted to plain `https://`. Git remote helpers such as `ext::`
are command execution, and `file://` would point back at attacker-writable disk.

## What it does on every apply

1. Refuse if the live file has drifted from the hash last applied. A hand edit
   is load-bearing until proven otherwise; silently overwriting it is how two
   copies of a config diverge in the first place. Clear a refusal with
   `--adopt` once the live state is reconciled or accepted.
2. Snapshot the current file into `/var/lib/homeboy-edge/snapshots/`.
3. Install the new content, then run `VALIDATE_CMD`.
4. Run `RELOAD_CMD`, then every `PROBE` (three attempts, two seconds apart),
   comparing the observed HTTP status to the expected one. The optional third
   field is passed to `curl --resolve`, to probe the origin directly rather than
   whatever a CDN has cached.
5. Roll back, revalidate, and reload again if any step past the write fails.
6. Record the applied hash and log before/after hashes to syslog.

Applying content identical to what is live is a no-op: state is recorded, and
nothing is reloaded.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Applied, or already current |
| 1 | Apply failed; the previous file was restored |
| 2 | Usage or configuration error; nothing was touched |
| 3 | Refused: live file drifted from last applied state |
| 4 | Refused: requested commit is not merged into the trust ref |

## Tests

`scripts/test-homeboy-edge-apply.sh` runs on Linux against a local fixture
repository and asserts the properties that matter: hostile content written into
a checkout is ignored, unmerged commits are refused, validation and probe
failures roll back, and drift blocks an apply until adopted.

## Operational notes

- `VALIDATE_CMD` and `RELOAD_CMD` are split on whitespace, not evaluated by a
  shell. Point them at a script if you need shell semantics.
- An unrecognised config key is a hard error, so a config written for a newer
  wrapper never silently skips a validation step on an older one.
- The wrapper is the applying authority for its `DEST_PATH`. Keep that file's
  source of truth in the repository, and reconcile rather than hand-edit.
