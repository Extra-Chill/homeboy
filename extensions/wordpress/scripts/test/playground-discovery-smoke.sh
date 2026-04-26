#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/playground-runner.php"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

mkdir -p "${TMPDIR}/component/tests"
cat > "${TMPDIR}/phpunit.xml.dist" <<'XML'
<phpunit>
    <testsuites>
        <testsuite name="Component Tests">
            <directory>tests/</directory>
        </testsuite>
        <testsuite name="Extension Self Tests">
            <directory suffix="UnitTest.php">HomeboyWordPress/Tests/</directory>
        </testsuite>
    </testsuites>
</phpunit>
XML

php -d display_errors=1 -r '
$runner = $argv[1];
$xml_path = $argv[2];
$test_dir = $argv[3];
$source = file_get_contents($runner);

function extract_function_source($source, $name) {
    $needle = "function {$name}";
    $start = strpos($source, $needle);
    if ($start === false) {
        fwrite(STDERR, "Missing function {$name}\n");
        exit(1);
    }
    $brace = strpos($source, "{", $start);
    $depth = 0;
    $len = strlen($source);
    for ($i = $brace; $i < $len; $i++) {
        if ($source[$i] === "{") {
            $depth++;
        } elseif ($source[$i] === "}") {
            $depth--;
            if ($depth === 0) {
                return substr($source, $start, $i - $start + 1);
            }
        }
    }
    fwrite(STDERR, "Unterminated function {$name}\n");
    exit(1);
}

$logs = [];
function pg_log($message) {
    global $logs;
    $logs[] = $message;
}

eval(extract_function_source($source, "pg_is_component_phpunit_directory"));
eval(extract_function_source($source, "pg_parse_phpunit_config"));

[$dirs, $suffixes, $prefixes, $excludes] = pg_parse_phpunit_config($xml_path, $test_dir);

$expected_dir = dirname($test_dir) . "/tests";
$bad_dir = dirname($test_dir) . "/HomeboyWordPress/Tests";

$assertions = 0;
$assert = function ($condition, $message) use (&$assertions) {
    $assertions++;
    if (!$condition) {
        fwrite(STDERR, "FAIL: {$message}\n");
        exit(1);
    }
};

$assert(in_array($expected_dir, $dirs, true), "keeps component tests directory");
$assert(!in_array($bad_dir, $dirs, true), "does not project extension tests into component root");
$assert($suffixes === ["Test.php"], "ignored extension-only suffixes");
$assert(pg_is_component_phpunit_directory("tests/Unit"), "accepts nested component tests directory");
$assert(!pg_is_component_phpunit_directory("HomeboyWordPress/Tests"), "rejects extension internal directory");

echo "Playground discovery smoke passed ({$assertions} assertions)\n";
' "$RUNNER" "${TMPDIR}/phpunit.xml.dist" "${TMPDIR}/component/tests"
