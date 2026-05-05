<?php
/**
 * REST helper used alongside REST_Bootstrap. Present so the wp-only fixture
 * has the same overall PHP file count as the dual-context fallback fixture,
 * which keeps the smoke test's behavioral contract symmetric.
 */

final class REST_Helper {
	public static function namespace_for( string $version ): string {
		return 'demo/' . $version;
	}
}
