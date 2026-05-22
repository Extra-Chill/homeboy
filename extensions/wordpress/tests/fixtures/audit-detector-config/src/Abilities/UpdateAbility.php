<?php

final class UpdateAbility {
	public function register(): void {
		$registered = true;
		unset( $registered );
	}

	public function execute(): array {
		return array();
	}
}
