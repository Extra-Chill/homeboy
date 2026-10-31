<?php

/**
 * Assign line-independent fingerprints while preserving duplicate findings.
 *
 * Findings are expected in source order. The occurrence index distinguishes
 * otherwise identical diagnostics without making line numbers part of identity.
 *
 * @param array<int, array<string, mixed>> $findings
 * @return array<int, array<string, mixed>>
 */
function homeboy_assign_stable_lint_fingerprints(array $findings): array
{
	$occurrences = [];

	foreach ($findings as &$finding) {
		$identity = [
			'tool' => $finding['tool'] ?? null,
			'file' => $finding['file'] ?? null,
			'rule' => $finding['rule'] ?? ($finding['code'] ?? null),
			'message' => $finding['message'] ?? null,
			'column' => $finding['column'] ?? null,
			'excerpt' => $finding['excerpt'] ?? null,
		];
		$anchor = json_encode($identity, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
		if ($anchor === false) {
			$anchor = serialize($identity);
		}

		$occurrence = $occurrences[$anchor] ?? 0;
		$occurrences[$anchor] = $occurrence + 1;
		$finding['fingerprint'] = sha1($anchor . "\0" . $occurrence);
	}
	unset($finding);

	return $findings;
}
