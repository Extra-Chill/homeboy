#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT_DIR/wordpress/wordpress.json"

php -r 'json_decode(file_get_contents($argv[1]), true, 512, JSON_THROW_ON_ERROR);' "$MANIFEST"

php -r '
$manifest = json_decode(file_get_contents($argv[1]), true, 512, JSON_THROW_ON_ERROR);
$overrides = $manifest["deploy"]["overrides"] ?? [];
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

    $command = $match["install_command"] ?? "";
    $cleanup = $match["cleanup_command"] ?? "";
    $slugVar = $kind . "_slug";

    $required = [
        "$slugVar=\$(basename {{targetDir}})",
        "zip_root=\$(unzip -Z1 {{stagingArtifact}}",
        "if [ \"\$zip_root\" != \"\$$slugVar\" ]",
        "rm -rf {{targetDir}}",
        "mkdir -p {{targetParentDir}}",
        "unzip -oq {{stagingArtifact}} -d {{targetParentDir}}",
        "test -d {{targetDir}}",
    ];

    foreach ($required as $needle) {
        if (!str_contains($command, $needle)) {
            fwrite(STDERR, "Expected $kind install command to contain: $needle\n");
            exit(1);
        }
    }

    $forbidden = [
        "mv {{targetDir}} {{targetDir}}.bak",
        "mv \"\$extracted\" {{targetDir}}",
        "cp -R",
        "cp -a",
        "{{targetDir}}.bak",
        "mktemp -d",
    ];

    foreach ($forbidden as $needle) {
        if (str_contains($command, $needle) || str_contains($cleanup, $needle)) {
            fwrite(STDERR, "Expected $kind commands not to contain: $needle\n");
            exit(1);
        }
    }

    if ($cleanup !== "rm -f {{stagingArtifact}}") {
        fwrite(STDERR, "Unexpected $kind cleanup command: $cleanup\n");
        exit(1);
    }
}
' "$MANIFEST"

echo "wordpress install command smoke passed"
