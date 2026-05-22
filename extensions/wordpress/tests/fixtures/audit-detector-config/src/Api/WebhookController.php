<?php

namespace Demo\Api;

final class WebhookController {
	public function register(): void {
		add_action( 'rest_api_init', array( $this, 'register_routes' ) );
	}

	public function register_routes(): void {
		register_rest_route( 'demo/v1', '/webhook/status', array(
			'methods'  => 'GET',
			'callback' => array( $this, 'dispatch' ),
		) );
	}

	public function dispatch( array $request ): array {
		return $request;
	}
}
