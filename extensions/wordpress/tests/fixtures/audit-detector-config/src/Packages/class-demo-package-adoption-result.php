<?php
/**
 * Adoption result — distinct constructor shape (status-driven), separate role
 * from the package value-object family.
 */

final class Demo_Package_Adoption_Result {
	public function __construct(
		public readonly string $status,
		public readonly ?string $adopted_slug = null,
		public readonly array $messages = array()
	) {}
}
