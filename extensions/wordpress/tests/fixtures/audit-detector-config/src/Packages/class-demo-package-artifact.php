<?php
/**
 * Artifact declaration object — distinct constructor shape from the package
 * manifest family. Adopters pass a single declaration array describing one
 * artifact; lumping this constructor into the manifest convention would
 * pollute it with an unrelated role.
 */

final class Demo_Package_Artifact {
	public function __construct(
		public readonly array $declaration
	) {}

	public function to_array(): array {
		return $this->declaration;
	}
}
