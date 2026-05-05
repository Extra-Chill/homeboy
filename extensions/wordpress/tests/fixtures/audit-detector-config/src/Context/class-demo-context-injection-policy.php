<?php
/**
 * Context policy vocabulary/configuration values.
 */

final class Demo_Context_Injection_Policy {
 public const ALWAYS    = 'always';
 public const ON_INTENT = 'on_intent';
 public const MANUAL    = 'manual';

 public static function values(): array {
 	return array( self::ALWAYS, self::ON_INTENT, self::MANUAL );
 }

 public static function normalize( string $policy ): string {
 	return in_array( $policy, self::values(), true ) ? $policy : self::ALWAYS;
 }
}
