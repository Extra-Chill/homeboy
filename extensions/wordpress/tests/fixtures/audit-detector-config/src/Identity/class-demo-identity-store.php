<?php
/**
 * Identity store contract — persistence interface, NOT a value object. Must
 * not be flagged as missing `key()` because it sits next to two value-object
 * siblings that happen to share that method.
 */

interface Demo_Identity_Store {
	public function resolve( Demo_Identity_Scope $scope ): ?Demo_Materialized_Identity;
}
