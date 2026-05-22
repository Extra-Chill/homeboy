<?php

namespace Demo\Api;

final class WebhookVerifier {
	public function verify( string $payload, string $signature ): WebhookVerificationResult {
		return new WebhookVerificationResult( '' !== $payload && '' !== $signature );
	}
}
