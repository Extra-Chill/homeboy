<?php
/**
 * Artifact type marker — value-object family alongside the package and
 * artifact siblings.
 */

final class Demo_Package_Artifact_Type {
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
