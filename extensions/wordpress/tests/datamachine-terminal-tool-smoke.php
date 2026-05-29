<?php
declare(strict_types=1);

$root = dirname(__DIR__);
$workload = $root . '/scripts/agent/datamachine-agent-workload.php';
$source = file_get_contents($workload);
if (false === $source) {
	fwrite(STDERR, "Unable to read workload.\n");
	exit(1);
}

$needle = 'class Homeboy_Datamachine_Agent_Terminal_Tool';
$start = strpos($source, $needle);
if (false === $start) {
	fwrite(STDERR, "Terminal tool class not found.\n");
	exit(1);
}

$brace = strpos($source, '{', $start);
$depth = 0;
$end = null;
for ($index = $brace; $index < strlen($source); $index++) {
	$char = $source[$index];
	if ('{' === $char) {
		$depth++;
	} elseif ('}' === $char) {
		$depth--;
		if (0 === $depth) {
			$end = $index + 1;
			break;
		}
	}
}
if (null === $end) {
	fwrite(STDERR, "Terminal tool class end not found.\n");
	exit(1);
}

function wp_json_encode($value): string {
	return (string) json_encode($value);
}

function wp_remote_post(string $url, array $args) {
	$headers = '';
	foreach (($args['headers'] ?? array()) as $name => $value) {
		$headers .= $name . ': ' . $value . "\r\n";
	}
	$context = stream_context_create(
		array(
			'http' => array(
				'method'        => 'POST',
				'header'        => $headers,
				'content'       => (string) ($args['body'] ?? ''),
				'timeout'       => (int) ($args['timeout'] ?? 30),
				'ignore_errors' => true,
			),
		)
	);
	$body = file_get_contents($url, false, $context);
	if (false === $body) {
		return array('error' => 'HTTP request failed');
	}

	$status = 0;
	foreach (($http_response_header ?? array()) as $header) {
		if (preg_match('/^HTTP\/\S+\s+(\d+)/', $header, $matches)) {
			$status = (int) $matches[1];
			break;
		}
	}

	return array(
		'response' => array('code' => $status),
		'body'     => $body,
	);
}

function is_wp_error($value): bool {
	return is_array($value) && isset($value['error']);
}

function wp_remote_retrieve_response_code(array $response): int {
	return (int) ($response['response']['code'] ?? 0);
}

function wp_remote_retrieve_body(array $response): string {
	return (string) ($response['body'] ?? '');
}

eval(substr($source, $start, $end - $start));

$runtime_dir = sys_get_temp_dir() . '/homeboy-php-terminal-tool-' . bin2hex(random_bytes(4));
$bin_dir = $runtime_dir . '/bin';
mkdir($bin_dir, 0777, true);
file_put_contents($bin_dir . '/wp', "#!/usr/bin/env bash\nprintf 'php-tool-wp:%s\\n' \"$*\"\n");
chmod($bin_dir . '/wp', 0755);

$ready_file = $runtime_dir . '/ready.json';
$token = 'php-smoke-token';
$node = trim((string) shell_exec('command -v node'));
if ('' === $node) {
	fwrite(STDERR, "node is required for terminal action server smoke.\n");
	exit(1);
}
$command = sprintf(
	'PATH=%s:$PATH %s %s --runtime-root %s --ready-file %s --token %s >%s 2>&1 & echo $!',
	escapeshellarg($bin_dir),
	escapeshellarg($node),
	escapeshellarg($root . '/scripts/agent/terminal-action-server.js'),
	escapeshellarg($runtime_dir),
	escapeshellarg($ready_file),
	escapeshellarg($token),
	escapeshellarg($runtime_dir . '/server.log')
);
$pid = (int) shell_exec($command);

try {
	$deadline = microtime(true) + 10;
	while (! is_file($ready_file) && microtime(true) < $deadline) {
		usleep(50000);
	}
	if (! is_file($ready_file)) {
		fwrite(STDERR, "Terminal action server did not become ready.\n" . (string) @file_get_contents($runtime_dir . '/server.log'));
		exit(1);
	}

	$ready = json_decode((string) file_get_contents($ready_file), true);
	$tool = new Homeboy_Datamachine_Agent_Terminal_Tool();
	$result = $tool->handle_tool_call(
		array('command' => 'option get blogname'),
		array(
			'tool_name'             => 'run_wp_cli',
			'terminal_action_url'   => $ready['url'],
			'terminal_action_token' => $token,
			'terminal_action_type'  => 'wp_cli',
		)
	);

	if (empty($result['success']) || 'wp option get blogname' !== ($result['command'] ?? '') || ! str_contains((string) ($result['stdout'] ?? ''), 'php-tool-wp:option get blogname')) {
		fwrite(STDERR, "Unexpected terminal tool result:\n" . json_encode($result, JSON_PRETTY_PRINT) . "\n");
		exit(1);
	}

	class WP_CLI {
		public static array $commands = array();

		public static function runcommand($command, $options = array()) {
			self::$commands[] = array($command, $options);
			return 'runtime-wp:' . $command;
		}
	}

	$runtime_result = $tool->handle_tool_call(
		array('command' => 'wp plugin list --format=table'),
		array(
			'tool_name'            => 'run_wp_cli',
			'terminal_action_type' => 'wp_cli',
		)
	);

	if (empty($runtime_result['success']) || 'wp plugin list --format=table' !== ($runtime_result['command'] ?? '') || ! str_contains((string) ($runtime_result['stdout'] ?? ''), 'runtime-wp:plugin list --format=table')) {
		fwrite(STDERR, "Unexpected runtime WP-CLI result:\n" . json_encode($runtime_result, JSON_PRETTY_PRINT) . "\n");
		exit(1);
	}

	if ('plugin list --format=table' !== (WP_CLI::$commands[0][0] ?? '') || true !== (WP_CLI::$commands[0][1]['return'] ?? null) || false !== (WP_CLI::$commands[0][1]['launch'] ?? null)) {
		fwrite(STDERR, "Unexpected WP_CLI::runcommand call:\n" . json_encode(WP_CLI::$commands, JSON_PRETTY_PRINT) . "\n");
		exit(1);
	}

	fwrite(STDOUT, "Data Machine terminal tool smoke passed.\n");
} finally {
	if ($pid > 0) {
		exec('kill ' . (int) $pid . ' >/dev/null 2>&1 || true');
	}
	exec('rm -rf ' . escapeshellarg($runtime_dir));
}
