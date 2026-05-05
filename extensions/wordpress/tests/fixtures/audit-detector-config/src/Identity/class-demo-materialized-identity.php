<?php
/**
 * Materialized identity value object — sibling of the scope above.
 */

final class Demo_Materialized_Identity {
	public function __construct(
		public readonly int $identity_id,
		public readonly string $owner_id,
		public readonly string $tenant
	) {}

	public function key(): string {
		return $this->owner_id . ':' . $this->tenant;
	}
}
