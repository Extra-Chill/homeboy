<?php
/**
 * Identity scope value object — declares a `key()` method shared by sibling
 * value objects. The `class-*-store.php` interface in the same directory must
 * not be forced into this method convention.
 */

final class Demo_Identity_Scope {
	public function __construct( public readonly string $owner_id, public readonly string $tenant ) {}

	public function key(): string {
		return $this->owner_id . ':' . $this->tenant;
	}
}
