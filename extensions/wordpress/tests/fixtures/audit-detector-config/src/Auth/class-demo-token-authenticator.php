<?php
/**
 * Request-edge auth service; not an auth DTO/value object.
 */

final class Demo_Token_Authenticator {
 public function __construct(
 	private readonly Demo_Token_Store $token_store
 ) {}

 public function authenticate_bearer_token( string $raw_token ): ?Demo_Token {
 	$raw_token = trim( $raw_token );
 	if ( '' === $raw_token ) {
 		return null;
 	}

 	return $this->token_store->resolve_token_hash( hash( 'sha256', $raw_token ) );
 }
}
