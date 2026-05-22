<?php

namespace Demo\Api;

final class WebhookRoutes {
	public function register(): void {
		add_action( 'rest_api_init', array( $this, 'register_routes' ) );
	}

	public function register_routes(): void {
		register_rest_route( 'demo/v1', '/webhook', array(
			'methods'  => 'POST',
			'callback' => array( $this, 'handle' ),
		) );
	}

	public function handle( array $request ): array {
		return $request;
	}
}
