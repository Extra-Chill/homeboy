<?php

namespace Demo\Api;

final class WebhookVerificationResult {
	public function __construct(
		public readonly bool $verified,
		public readonly string $reason = ''
	) {}
}
