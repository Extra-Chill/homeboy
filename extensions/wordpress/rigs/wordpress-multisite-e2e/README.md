# WordPress Multisite E2E Rig

This portable Homeboy rig boots a disposable path-based WordPress multisite in
WP Codebox. Its synthetic fixture creates a main site plus `/alpha/` and
`/beta/`, network-activates a network-only plugin, and verifies shared user
identity, isolated site options/content, shared network state, anonymous browser
access, authenticated cross-site navigation, console/page errors, assertions,
screenshots, and browser evidence artifacts.

The rig composes existing ownership boundaries: Homeboy installs and runs the
package, the WordPress extension supplies WordPress-specific fixture composition,
and WP Codebox owns Playground, browser automation, runtime policy, and evidence.

## Install And Validate

From a homeboy-extensions checkout:

```bash
homeboy rig install ./wordpress/rigs/wordpress-multisite-e2e
homeboy rig materialize ./wordpress/rigs/wordpress-multisite-e2e/rig.json
homeboy rig lint ./wordpress/rigs/wordpress-multisite-e2e
homeboy rig package lint ./wordpress/rigs/wordpress-multisite-e2e
homeboy rig check wordpress-multisite-e2e
```

Run the real disposable network integration:

```bash
HOMEBOY_ARTIFACT_ROOT="$PWD/artifacts/wordpress-multisite-e2e" \
  homeboy rig up wordpress-multisite-e2e
```

To verify against an unreleased WP Codebox checkout, use an explicit executable
override without changing committed config:

```bash
HOMEBOY_WP_CODEBOX_BIN=/path/to/wp-codebox/packages/cli/dist/index.js \
HOMEBOY_ARTIFACT_ROOT="$PWD/artifacts/wordpress-multisite-e2e" \
  homeboy rig up wordpress-multisite-e2e
```

## Consumer Configuration

The runner reads the WordPress extension's existing `HOMEBOY_SETTINGS_JSON`
settings. Consumers do not edit the rig:

- `wordpress_runtime_blueprint` adds ordinary WordPress blueprint setup while
  the rig ensures `enableMultisite` remains present.
- `wp_codebox_extra_plugins` mounts consumer plugins/components.
- `wp_codebox_extra_themes` mounts immutable local theme checkouts readonly.
  Entries use `{source,slug,activate?,metadata?}`; sources must be absolute paths
  to directories with a non-empty `style.css` `Theme Name` header in the first
  8 KB. Standalone themes need a WordPress-supported index template; child themes
  need their standalone parent in the same mount list. At most one theme may be
  active, and supplied metadata remains in WP Codebox recipe evidence.
- `wordpress_runtime_prepare_steps` adds seed/setup recipe steps.
- `wordpress_runtime_workloads` runs consumer workloads through
  `wordpress.bench` after network setup.
- `wp_codebox_scenario_manifests` adds inline or file-backed browser journeys.
- `wordpress_runtime_post_steps` adds final assertions or evidence steps.
- `wordpress_runtime_version` pins the disposable WordPress version.
- `wordpress_runtime_php_version` pins the rig's WP Codebox PHP runtime using a
  supported `major.minor` value such as `8.4`.

Example:

```bash
HOMEBOY_SETTINGS_JSON='{
  "wp_codebox_extra_plugins": [
    {"source":"/absolute/path/to/plugin","slug":"plugin-under-test","activate":true}
  ],
  "wp_codebox_extra_themes": [
    {
      "source":"/absolute/path/to/theme",
      "slug":"theme-under-test",
      "activate":true,
      "metadata":{"provenance":{"revision":"full-immutable-revision"}}
    }
  ],
  "wordpress_runtime_php_version":"8.4",
  "wordpress_runtime_prepare_steps": [
    {"command":"wordpress.wp-cli","args":["command=option update fixture_ready yes --url=http://localhost/alpha/"]}
  ],
  "wp_codebox_scenario_manifests": ["/absolute/path/to/browser-journey.json"]
}' homeboy rig up wordpress-multisite-e2e
```

Use absolute consumer paths when a package is installed from another checkout.
WP Codebox validates and executes all supplied recipe steps with its normal
runtime policy and artifact handling.

The active theme becomes `WP_DEFAULT_THEME` for sites created later in the
workflow, and the rig switches all sites that exist after its network seed. This
contract is specific to `wordpress-multisite-e2e`: the canonical WP Codebox bench
builder does not currently expose PHP runtime selection.

## Boundary

This rig covers native path-based Playground multisite at the canonical
`http://localhost` origin. It does not claim mapped-subdomain, mapped-domain, or
cross-domain-cookie parity. Those topologies require a separate runtime that can
faithfully model DNS, origins, TLS, and browser cookie boundaries.
