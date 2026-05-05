<?php
/**
 * Plugin Name: WP Only Plugin
 * Description: Fixture exercising guards around symbols that only ship with WordPress.
 * Requires at least: 6.0
 * Requires PHP: 8.1
 * Author: Extra Chill
 * License: GPL-2.0-or-later
 *
 * The shape under test:
 *
 * - Production source guards a WordPress-only class (WP_REST_Server) and a
 *   WordPress-only function (register_rest_route) outside any lifecycle/test
 *   path. Neither symbol has a sensible pure-PHP fallback.
 * - The WordPress extension keeps these symbols in known_symbols, so core's
 *   dead_guard detector should still flag the guards as redundant.
 */

require_once __DIR__ . '/src/Runtime/class-rest-bootstrap.php';
