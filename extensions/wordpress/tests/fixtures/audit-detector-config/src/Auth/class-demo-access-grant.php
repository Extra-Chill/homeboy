<?php
/**
 * Auth value object with stable JSON export helpers.
 */

final class Demo_Access_Grant {
 public function __construct(
 	public readonly string $agent_id,
 	public readonly int $user_id,
 	public readonly array $metadata = array()
 ) {}

 public static function from_array( array $grant ): self {
 	return new self(
 		isset( $grant['agent_id'] ) ? (string) $grant['agent_id'] : '',
 		isset( $grant['user_id'] ) ? (int) $grant['user_id'] : 0,
 		isset( $grant['metadata'] ) && is_array( $grant['metadata'] ) ? $grant['metadata'] : array()
 	);
 }

 public function to_array(): array {
 	return array(
 		'agent_id' => $this->agent_id,
 		'user_id'  => $this->user_id,
 		'metadata' => $this->metadata,
 	);
 }
}
