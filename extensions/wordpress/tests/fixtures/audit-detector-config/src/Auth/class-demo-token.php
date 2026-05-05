<?php
/**
 * Auth value object that intentionally exposes redacted metadata, not full storage rows.
 */

final class Demo_Token {
 public function __construct(
 	public readonly int $token_id,
 	public readonly string $token_hash,
 	public readonly string $token_prefix,
 	public readonly array $metadata = array()
 ) {}

 public static function from_array( array $token ): self {
 	return new self(
 		isset( $token['token_id'] ) ? (int) $token['token_id'] : 0,
 		isset( $token['token_hash'] ) ? (string) $token['token_hash'] : '',
 		isset( $token['token_prefix'] ) ? (string) $token['token_prefix'] : '',
 		isset( $token['metadata'] ) && is_array( $token['metadata'] ) ? $token['metadata'] : array()
 	);
 }

 public function to_metadata_array(): array {
 	return array(
 		'token_id'     => $this->token_id,
 		'token_prefix' => $this->token_prefix,
 		'metadata'     => $this->metadata,
 	);
 }
}
