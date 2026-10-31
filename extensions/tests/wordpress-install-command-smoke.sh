#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT_DIR/wordpress/wordpress.json"

php -r 'json_decode(file_get_contents($argv[1]), true, 512, JSON_THROW_ON_ERROR);' "$MANIFEST"

php -r '
$manifest = json_decode(file_get_contents($argv[1]), true, 512, JSON_THROW_ON_ERROR);
$overrides = $manifest["deploy"]["archive_install"] ?? [];
$expected = [
    "/wp-content/plugins/" => "plugin",
    "/wp-content/themes/" => "theme",
];

foreach ($expected as $pattern => $kind) {
    $match = null;
    foreach ($overrides as $override) {
        if (($override["path_pattern"] ?? "") === $pattern) {
            $match = $override;
            break;
        }
    }

    if (!$match) {
        fwrite(STDERR, "Missing override for $pattern\n");
        exit(1);
    }

    if (($match["staging_path"] ?? "") !== "/tmp/homeboy-staging") {
        fwrite(STDERR, "Unexpected $kind staging path\n");
        exit(1);
    }

    if (($match["root_must_match_target_basename"] ?? null) !== true) {
        fwrite(STDERR, "Expected $kind archive install to require root basename match\n");
        exit(1);
    }

    $requiredHeader = $match["required_header"] ?? [];
    if ($kind === "plugin" && (($requiredHeader["file"] ?? "") !== "{{targetBasename}}.php" || ($requiredHeader["contains"] ?? "") !== "Plugin Name:")) {
        fwrite(STDERR, "Unexpected plugin required header\n");
        exit(1);
    }
    if ($kind === "theme" && (($requiredHeader["file"] ?? "") !== "style.css" || ($requiredHeader["contains"] ?? "") !== "Theme Name:")) {
        fwrite(STDERR, "Unexpected theme required header\n");
        exit(1);
    }
}
' "$MANIFEST"

echo "wordpress install command smoke passed"
