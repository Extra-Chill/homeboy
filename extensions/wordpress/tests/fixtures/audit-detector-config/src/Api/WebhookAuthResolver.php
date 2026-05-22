<?php

namespace Demo\Api;

final class WebhookAuthResolver {
	public static function resolve( array $config ): array {
		return array_filter( $config );
	}
}
