<?php
/**
 * Package value object — declarative manifest. Sibling of
 * `class-demo-package-artifact-type.php` (slug/type + args ctor shape); together
 * they form a small value-object family the detector should not pollute with
 * artifact declarations, adopter contracts, registries, or result/diff objects.
 */

final class Demo_Package {
	public function __construct(
		public readonly string $slug,
		public readonly array $args = array()
	) {}

	public function to_array(): array {
		return array(
			'slug' => $this->slug,
			'args' => $this->args,
		);
	}
}
