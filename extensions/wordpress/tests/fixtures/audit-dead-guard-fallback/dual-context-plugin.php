<?php
/**
 * Plugin Name: Dual Context Plugin
 * Description: Fixture exercising WordPress + pure-PHP fallback guards.
 * Requires at least: 6.0
 * Requires PHP: 8.1
 * Author: Extra Chill
 * License: GPL-2.0-or-later
 *
 * The shape under test:
 *
 * - Production source under src/ defensively guards WordPress utility helpers
 *   that have natural pure-PHP equivalents (wp_json_encode, wp_generate_uuid4).
 * - The library is intended to load both inside WordPress and inside pure-PHP
 *   smoke tests, so the guard's else branch is reached for real.
 * - The WordPress extension's audit.detector_rules.known_symbols list must not
 *   declare these utility helpers as guaranteed at runtime, otherwise core's
 *   dead_guard detector will report the guards as dead.
 */

require_once __DIR__ . '/src/Runtime/class-dual-context-encoder.php';
require_once __DIR__ . '/src/Runtime/class-dual-context-identifier.php';
