<?php
/**
 * Package value object — declarative manifest. Sibling of the artifact value
 * objects below; together they form a small constructor convention that the
 * detector should NOT pollute with adopter/registry/result roles.
 */

final class Demo_Package {
	public function __construct(
		public readonly string $slug,
		public readonly array $definition,
		public readonly array $artifacts
	) {}

	public function to_array(): array {
		return array(
			'slug'       => $this->slug,
			'definition' => $this->definition,
			'artifacts'  => $this->artifacts,
		);
	}
}
