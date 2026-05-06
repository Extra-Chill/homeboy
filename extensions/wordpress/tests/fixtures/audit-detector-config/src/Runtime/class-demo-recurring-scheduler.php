<?php
/**
 * Demo Recurring Scheduler — fixture for issue #425.
 *
 * Exercises the constant-backed slug literal detector source pattern. The
 * docblock intentionally contains the phrase "This class has no knowledge"
 * because the original (?s) source pattern would non-greedily capture
 * `class has` from the prose and walk forward to the next `const`,
 * reporting a bogus `has::GROUP` constant.
 *
 * After the fix, the source pattern is anchored to start of line (so a
 * docblock-continuation line beginning with `*` cannot satisfy `class`),
 * AND it requires an opening brace before `const` so the match must live
 * inside an actual class body.
 *
 * This class has no knowledge of flows, jobs, tasks, or settings.
 */

final class Demo_Recurring_Scheduler {
	public const GROUP = 'demo-recurring';

	public function dispatch(): array {
		return array(
			'group' => 'demo-recurring',
		);
	}
}
