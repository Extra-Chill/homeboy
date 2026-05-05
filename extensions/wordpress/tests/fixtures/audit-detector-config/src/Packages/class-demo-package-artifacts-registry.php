<?php
/**
 * Artifacts registry — singleton service role, distinct from value objects,
 * results, and adopter contracts.
 */

final class Demo_Package_Artifacts_Registry {
	private static ?self $instance = null;

	/** @var array<string, Demo_Package_Artifact_Type> */
	private array $types = array();

	private function __construct() {}

	public static function instance(): self {
		if ( null === self::$instance ) {
			self::$instance = new self();
		}
		return self::$instance;
	}

	public function register( Demo_Package_Artifact_Type $type ): void {
		$this->types[ $type->slug ] = $type;
	}
}
