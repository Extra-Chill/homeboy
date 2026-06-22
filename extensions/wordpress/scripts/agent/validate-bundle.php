#!/usr/bin/env php
<?php
declare(strict_types=1);

/**
 * Validate an agent bundle from a thin JSON spec.
 *
 * Usage:
 *   php wordpress/scripts/agent/validate-bundle.php path/to/spec.json
 *
 * Spec example:
 *   {
 *     "bundle_dir": "bundles/example-agent",
 *     "bundle_slug": "example-agent",
 *     "agent_slug": "example-agent",
 *     "agent_label": "Example Agent",
 *     "expected_pipelines": ["example-pipeline"],
 *     "expected_flows": ["example-flow"],
 *     "memory_files": ["SOUL.md", "MEMORY.md"],
 *     "manifest_assertions": {
 *       "run_artifacts.completion_assertions.egress": ["pr-body"],
 *       "agent.agent_config.daily_memory.enabled": true
 *     },
 *     "flow_assertions": {
 *       "example-flow": {
 *         "pipeline_slug": "example-pipeline",
 *         "ai_step_required_tools": [
 *           "get_github_file",
 *           "create_or_update_github_file",
 *           "create_github_pull_request"
 *         ],
 *         "completion_assertions_empty": true
 *       }
 *     },
 *     "pipeline_assertions": {
 *       "example-pipeline": {
 *         "system_prompt_must_contain": "source code"
 *       }
 *     },
 *     "example_runner_config": "examples/homeboy-runner-config.example.json",
 *     "example_assertions": {
 *       "success_requires_pr": false,
 *       "pipeline_slug": "example-pipeline"
 *     }
 *   }
 *
 * Paths:
 *   bundle_dir and example_runner_config are resolved relative to the spec
 *   file's parent directory. This lets consumers keep the spec at repo root
 *   with "bundle_dir": "bundles/example-agent", or under tests/ with
 *   "bundle_dir": "../bundles/example-agent".
 *
 * This script is intentionally standalone: no Composer dependencies, no
 * WordPress runtime, and no PHPUnit bootstrap.
 */

if (PHP_SAPI !== 'cli') {
    fwrite(STDERR, "ERROR: validate-bundle.php must be run from the CLI.\n");
    exit(1);
}

$spec_file = $argv[1] ?? '';
if ($spec_file === '' || in_array($spec_file, ['-h', '--help'], true)) {
    fwrite(STDERR, "Usage: php wordpress/scripts/agent/validate-bundle.php path/to/spec.json\n");
    exit($spec_file === '' ? 1 : 0);
}

$failures = 0;
$checks = 0;

$display = static function (mixed $value): string {
    if (is_string($value)) {
        return $value;
    }

    $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_PRETTY_PRINT);
    return $encoded === false ? var_export($value, true) : $encoded;
};

$fail = static function (string $label, mixed $expected, mixed $actual) use (&$failures, &$checks, $display): void {
    ++$checks;
    ++$failures;

    echo "  FAIL {$label}\n";
    echo '    expected: ' . $display($expected) . "\n";
    echo '    actual:   ' . $display($actual) . "\n";
};

$pass = static function (string $label) use (&$checks): void {
    ++$checks;
    echo "  PASS {$label}\n";
};

$assert_same = static function (string $label, mixed $expected, mixed $actual) use ($fail, $pass): void {
    if ($actual === $expected) {
        $pass($label);
        return;
    }

    $fail($label, $expected, $actual);
};

$assert_true = static function (string $label, bool $condition, mixed $expected, mixed $actual) use ($fail, $pass): void {
    if ($condition) {
        $pass($label);
        return;
    }

    $fail($label, $expected, $actual);
};

$resolve_path = static function (string $base_dir, string $path): string {
    if ($path !== '' && str_starts_with($path, '/')) {
        return $path;
    }

    return $base_dir . DIRECTORY_SEPARATOR . $path;
};

$read_json = static function (string $path, string $label): array {
    if (!is_file($path)) {
        fwrite(STDERR, "ERROR: {$label} not found at {$path}\n");
        exit(1);
    }

    $contents = file_get_contents($path);
    if ($contents === false) {
        fwrite(STDERR, "ERROR: unable to read {$label} at {$path}\n");
        exit(1);
    }

    $decoded = json_decode($contents, true);
    if (!is_array($decoded)) {
        fwrite(STDERR, "ERROR: malformed JSON in {$label} at {$path}: " . json_last_error_msg() . "\n");
        exit(1);
    }

    return $decoded;
};

$walk_path = static function (array $data, string $path): array {
    $current = $data;
    $walked = [];

    foreach (explode('.', $path) as $part) {
        $walked[] = $part;
        if (!is_array($current) || !array_key_exists($part, $current)) {
            return [false, implode('.', $walked), null];
        }

        $current = $current[$part];
    }

    return [true, $path, $current];
};

$find_first_ai_step = static function (array $data): ?array {
    $steps = $data['steps'] ?? null;
    if (!is_array($steps)) {
        return null;
    }

    foreach ($steps as $index => $step) {
        if (is_array($step) && ($step['step_type'] ?? null) === 'ai') {
            return [$index, $step];
        }
    }

    return null;
};

$spec_path = realpath($spec_file);
if ($spec_path === false) {
    fwrite(STDERR, "ERROR: spec not found at {$spec_file}\n");
    exit(1);
}

$spec_dir = dirname($spec_path);
$spec = $read_json($spec_path, 'spec');

foreach (['bundle_dir', 'bundle_slug', 'agent_slug'] as $required_key) {
    if (!array_key_exists($required_key, $spec) || !is_string($spec[$required_key]) || $spec[$required_key] === '') {
        fwrite(STDERR, "ERROR: spec missing required string key: {$required_key}\n");
        exit(1);
    }
}

$bundle_slug = $spec['bundle_slug'];
$bundle_dir = realpath($resolve_path($spec_dir, $spec['bundle_dir']));
if ($bundle_dir === false || !is_dir($bundle_dir)) {
    fwrite(STDERR, "ERROR: bundle_dir not found at " . $resolve_path($spec_dir, $spec['bundle_dir']) . "\n");
    exit(1);
}

echo "bundle-validator: {$bundle_slug}\n";

$manifest = $read_json($bundle_dir . DIRECTORY_SEPARATOR . 'manifest.json', 'manifest');

$assert_same('manifest bundle_slug matches spec', $bundle_slug, $manifest['bundle_slug'] ?? null);
$assert_same('manifest agent.slug matches spec', $spec['agent_slug'], $manifest['agent']['slug'] ?? null);
if (array_key_exists('agent_label', $spec)) {
    $assert_same('manifest agent.label matches spec', $spec['agent_label'], $manifest['agent']['label'] ?? null);
}

$included_pipelines = $manifest['included']['pipelines'] ?? [];
$included_flows = $manifest['included']['flows'] ?? [];
$loaded_pipelines = [];
$loaded_flows = [];

foreach (($spec['expected_pipelines'] ?? []) as $pipeline_slug) {
    $assert_true(
        "manifest includes pipeline {$pipeline_slug}",
        is_array($included_pipelines) && in_array($pipeline_slug, $included_pipelines, true),
        'included in manifest.included.pipelines',
        $included_pipelines
    );

    $pipeline_file = $bundle_dir . DIRECTORY_SEPARATOR . 'pipelines' . DIRECTORY_SEPARATOR . $pipeline_slug . '.json';
    $assert_true("pipeline file exists for {$pipeline_slug}", is_file($pipeline_file), $pipeline_file, is_file($pipeline_file) ? $pipeline_file : 'missing');
    if (is_file($pipeline_file)) {
        $loaded_pipelines[$pipeline_slug] = $read_json($pipeline_file, "pipeline {$pipeline_slug}");
        $assert_same("pipeline {$pipeline_slug} slug matches filename", $pipeline_slug, $loaded_pipelines[$pipeline_slug]['slug'] ?? null);
    }
}

foreach (($spec['expected_flows'] ?? []) as $flow_slug) {
    $assert_true(
        "manifest includes flow {$flow_slug}",
        is_array($included_flows) && in_array($flow_slug, $included_flows, true),
        'included in manifest.included.flows',
        $included_flows
    );

    $flow_file = $bundle_dir . DIRECTORY_SEPARATOR . 'flows' . DIRECTORY_SEPARATOR . $flow_slug . '.json';
    $assert_true("flow file exists for {$flow_slug}", is_file($flow_file), $flow_file, is_file($flow_file) ? $flow_file : 'missing');
    if (is_file($flow_file)) {
        $loaded_flows[$flow_slug] = $read_json($flow_file, "flow {$flow_slug}");
        $assert_same("flow {$flow_slug} slug matches filename", $flow_slug, $loaded_flows[$flow_slug]['slug'] ?? null);
        $pipeline_slug = $loaded_flows[$flow_slug]['pipeline_slug'] ?? null;
        $pipeline_file = is_string($pipeline_slug)
            ? $bundle_dir . DIRECTORY_SEPARATOR . 'pipelines' . DIRECTORY_SEPARATOR . $pipeline_slug . '.json'
            : null;
        $assert_true(
            "flow {$flow_slug} pipeline_slug points to an existing pipeline",
            $pipeline_file !== null && is_file($pipeline_file),
            'pipeline_slug referencing an existing pipeline file',
            $pipeline_slug
        );
    }
}

foreach (($spec['memory_files'] ?? []) as $memory_file) {
    $memory_path = $bundle_dir . DIRECTORY_SEPARATOR . 'memory' . DIRECTORY_SEPARATOR . 'agent' . DIRECTORY_SEPARATOR . $memory_file;
    $assert_true("memory file exists: {$memory_file}", is_file($memory_path), $memory_path, is_file($memory_path) ? $memory_path : 'missing');
}

foreach (($spec['manifest_assertions'] ?? []) as $path => $expected) {
    [$found, $missing_path, $actual] = $walk_path($manifest, (string) $path);
    if (!$found) {
        $fail("manifest assertion {$path}", $expected, "missing path: {$missing_path}");
        continue;
    }

    $assert_same("manifest assertion {$path}", $expected, $actual);
}

foreach (($spec['flow_assertions'] ?? []) as $flow_slug => $assertions) {
    if (!is_array($assertions)) {
        $fail("flow {$flow_slug} assertions are an object", 'object', gettype($assertions));
        continue;
    }

    if (!isset($loaded_flows[$flow_slug])) {
        $flow_file = $bundle_dir . DIRECTORY_SEPARATOR . 'flows' . DIRECTORY_SEPARATOR . $flow_slug . '.json';
        if (is_file($flow_file)) {
            $loaded_flows[$flow_slug] = $read_json($flow_file, "flow {$flow_slug}");
        } else {
            $fail("flow assertion target exists: {$flow_slug}", $flow_file, 'missing');
            continue;
        }
    }

    $flow = $loaded_flows[$flow_slug];
    if (array_key_exists('pipeline_slug', $assertions)) {
        $assert_same("flow {$flow_slug} pipeline_slug", $assertions['pipeline_slug'], $flow['pipeline_slug'] ?? null);
    }

    if (array_key_exists('ai_step_required_tools', $assertions)) {
        $ai_step = $find_first_ai_step($flow);
        if ($ai_step === null) {
            $fail("flow {$flow_slug} has an AI step", 'first step with step_type=ai', 'missing');
        } else {
            [$step_index, $step] = $ai_step;
            $enabled_tools = $step['enabled_tools'] ?? [];
            foreach ($assertions['ai_step_required_tools'] as $tool) {
                $assert_true(
                    "flow {$flow_slug} AI step {$step_index} enables tool {$tool}",
                    is_array($enabled_tools) && in_array($tool, $enabled_tools, true),
                    'enabled in steps[' . $step_index . '].enabled_tools',
                    $enabled_tools
                );
            }
        }
    }

    if (($assertions['completion_assertions_empty'] ?? false) === true) {
        $ai_step = $find_first_ai_step($flow);
        if ($ai_step === null) {
            $fail("flow {$flow_slug} has an AI step for completion assertions", 'first step with step_type=ai', 'missing');
        } else {
            [$step_index, $step] = $ai_step;
            $completion_assertions = $step['completion_assertions'] ?? [];
            $assert_true(
                "flow {$flow_slug} AI step {$step_index} completion_assertions empty",
                $completion_assertions === [] || $completion_assertions === null,
                'empty or absent',
                $completion_assertions
            );
        }
    }
}

foreach (($spec['pipeline_assertions'] ?? []) as $pipeline_slug => $assertions) {
    if (!is_array($assertions)) {
        $fail("pipeline {$pipeline_slug} assertions are an object", 'object', gettype($assertions));
        continue;
    }

    if (!isset($loaded_pipelines[$pipeline_slug])) {
        $pipeline_file = $bundle_dir . DIRECTORY_SEPARATOR . 'pipelines' . DIRECTORY_SEPARATOR . $pipeline_slug . '.json';
        if (is_file($pipeline_file)) {
            $loaded_pipelines[$pipeline_slug] = $read_json($pipeline_file, "pipeline {$pipeline_slug}");
        } else {
            $fail("pipeline assertion target exists: {$pipeline_slug}", $pipeline_file, 'missing');
            continue;
        }
    }

    if (array_key_exists('system_prompt_must_contain', $assertions)) {
        $ai_step = $find_first_ai_step($loaded_pipelines[$pipeline_slug]);
        if ($ai_step === null) {
            $fail("pipeline {$pipeline_slug} has an AI step", 'first step with step_type=ai', 'missing');
        } else {
            [$step_index, $step] = $ai_step;
            $system_prompt = $step['step_config']['system_prompt'] ?? '';
            $needle = (string) $assertions['system_prompt_must_contain'];
            $assert_true(
                "pipeline {$pipeline_slug} AI step {$step_index} system_prompt contains {$needle}",
                is_string($system_prompt) && str_contains($system_prompt, $needle),
                "substring: {$needle}",
                $system_prompt
            );
        }
    }
}

if (array_key_exists('example_runner_config', $spec)) {
    if (!is_string($spec['example_runner_config']) || $spec['example_runner_config'] === '') {
        fwrite(STDERR, "ERROR: example_runner_config must be a non-empty string when provided.\n");
        exit(1);
    }

    $example_path = $resolve_path($spec_dir, $spec['example_runner_config']);
    $example_config = $read_json($example_path, 'example runner config');
    foreach (($spec['example_assertions'] ?? []) as $path => $expected) {
        [$found, $missing_path, $actual] = $walk_path($example_config, (string) $path);
        if (!$found) {
            $fail("example assertion {$path}", $expected, "missing path: {$missing_path}");
            continue;
        }

        $assert_same("example assertion {$path}", $expected, $actual);
    }
}

if ($failures > 0) {
    echo "\nFAILED: {$failures} validation checks failed.\n";
    exit(1);
}

echo "\nAll {$checks} validation checks passed.\n";
exit(0);
