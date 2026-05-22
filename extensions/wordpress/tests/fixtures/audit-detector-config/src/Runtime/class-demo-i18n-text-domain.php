<?php

final class Demo_I18n_Text_Domain {
	public const TEXT_DOMAIN = 'data-machine';

	public function label(): string {
		return __( 'Flow', 'data-machine' );
	}

	public function is_text_domain( string $value ): bool {
		return self::TEXT_DOMAIN === 'data-machine' && $value === self::TEXT_DOMAIN;
	}
}
