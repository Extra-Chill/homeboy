#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FINGERPRINT_HELPER="${SCRIPT_DIR}/stable-fingerprint.php"

php -r '
require $argv[1];

function make_findings(string $tool, int $lineOffset): array
{
    $codePrefix = $tool === "phpcs" ? "WordPress.Security" : "phpstan";
    $column = $tool === "phpcs" ? 9 : null;

    return [
        [
            "id" => "src/example.php::{$codePrefix}.unsafeCall::" . (10 + $lineOffset),
            "tool" => $tool,
            "file" => "src/example.php",
            "line" => 10 + $lineOffset,
            "column" => $column,
            "code" => "{$codePrefix}.unsafeCall",
            "rule" => "{$codePrefix}.unsafeCall",
            "message" => "Unsafe call ({$codePrefix}.unsafeCall)",
            "excerpt" => "dangerous_call();",
        ],
        [
            "id" => "src/example.php::{$codePrefix}.unsafeCall::" . (20 + $lineOffset),
            "tool" => $tool,
            "file" => "src/example.php",
            "line" => 20 + $lineOffset,
            "column" => $column,
            "code" => "{$codePrefix}.unsafeCall",
            "rule" => "{$codePrefix}.unsafeCall",
            "message" => "Unsafe call ({$codePrefix}.unsafeCall)",
            "excerpt" => "dangerous_call();",
        ],
        [
            "id" => "src/example.php::{$codePrefix}.unsafeCall::" . (30 + $lineOffset),
            "tool" => $tool,
            "file" => "src/example.php",
            "line" => 30 + $lineOffset,
            "column" => $column,
            "code" => "{$codePrefix}.unsafeCall",
            "rule" => "{$codePrefix}.unsafeCall",
            "message" => "Different unsafe call ({$codePrefix}.unsafeCall)",
            "excerpt" => "other_dangerous_call();",
        ],
    ];
}

foreach (["phpcs", "phpstan"] as $tool) {
    $original = homeboy_assign_stable_lint_fingerprints(make_findings($tool, 0));
    $shifted = homeboy_assign_stable_lint_fingerprints(make_findings($tool, 12));
    $originalFingerprints = array_column($original, "fingerprint");
    $shiftedFingerprints = array_column($shifted, "fingerprint");

    if ($originalFingerprints !== $shiftedFingerprints) {
        fwrite(STDERR, "FAIL: {$tool} fingerprints changed after insertion-only line movement\n");
        exit(1);
    }
    if (count(array_unique($originalFingerprints)) !== count($originalFingerprints)) {
        fwrite(STDERR, "FAIL: {$tool} distinct or repeated findings collapsed\n");
        exit(1);
    }
    if ($original[0]["id"] === $shifted[0]["id"] || $original[0]["line"] === $shifted[0]["line"]) {
        fwrite(STDERR, "FAIL: {$tool} fixture did not exercise line-based movement\n");
        exit(1);
    }
}
' "$FINGERPRINT_HELPER"

echo "stable lint fingerprint smoke passed"
