<?php
/**
 * Adoption diff — sibling result-shaped object.
 */

final class Demo_Package_Adoption_Diff {
	public function __construct(
		public readonly string $status,
		public readonly array $additions = array(),
		public readonly array $removals = array()
	) {}
}
