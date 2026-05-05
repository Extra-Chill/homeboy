<?php
/**
 * Artifact type marker — sibling of `class-demo-package.php` in the value-object
 * family. Matches the manifest's (slug/type, args) constructor shape so the
 * shared convention is detectable.
 */

final class Demo_Package_Artifact_Type {
	public function __construct(
		public readonly string $type,
		public readonly array $args = array()
	) {}

	public function to_array(): array {
		return array(
			'type' => $this->type,
			'args' => $this->args,
		);
	}
}
