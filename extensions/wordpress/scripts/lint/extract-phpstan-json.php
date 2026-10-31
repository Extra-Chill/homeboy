<?php

/**
 * Extract the PHPStan JSON report from a mixed stdout stream.
 *
 * `phpstan analyse --debug` prints one analysed file path per line to stdout
 * before the `--error-format=json` report. Depending on how the stream was
 * captured, the report may sit on its own line, be glued onto the final path
 * line (no newline in between), be followed by trailing output, or be missing
 * entirely when PHPStan died mid-analysis (memory limit, fatal error).
 *
 * Reads the stream from stdin. Prints the JSON report on a single line and
 * exits 0 when a complete PHPStan report (an object with a `totals` object) is
 * found; prints nothing and exits 1 otherwise. It never echoes non-JSON input,
 * so callers can treat empty output as "no report" (homeboy-extensions#2903).
 */

$input = stream_get_contents(STDIN);

if (!is_string($input) || $input === '') {
	exit(1);
}

$offset = 0;

while (($start = strpos($input, '{"totals"', $offset)) !== false) {
	$candidate = substr($input, $start);

	// The report is a single JSON document; trailing output after it must not
	// defeat the parse. Try the whole tail first, then just the first line.
	$attempts = [trim($candidate)];
	$lineEnd = strpos($candidate, "\n");

	if ($lineEnd !== false) {
		$attempts[] = trim(substr($candidate, 0, $lineEnd));
	}

	foreach ($attempts as $attempt) {
		$decoded = json_decode($attempt, true);

		if (is_array($decoded) && isset($decoded['totals']) && is_array($decoded['totals'])) {
			// Emit the original bytes, not a re-encode, so the report reaches
			// the summary and sidecar exactly as PHPStan wrote it.
			echo str_replace("\n", '', $attempt), "\n";
			exit(0);
		}
	}

	$offset = $start + 1;
}

exit(1);
