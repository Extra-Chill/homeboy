<?php
/**
 * Artifact value object inside a package. Same constructor shape family as the
 * package manifest above.
 */

final class Demo_Package_Artifact {
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
