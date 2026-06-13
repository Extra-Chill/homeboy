<?php
/**
 * Plugin Name:       Data Machine Agent CI Driver
 * Plugin URI:        https://github.com/Extra-Chill/homeboy-extensions
 * Description:       Path anchor plugin for Data Machine agent CI runs in WordPress Playground. Hosts no runtime behavior; mounted by the agent runner so workloads, bundles, and transcripts share a stable path.
 * Version:           1.0.0
 * Requires at least: 6.4
 * Requires PHP:      8.1
 * Author:            Extra Chill
 * Author URI:        https://github.com/Extra-Chill
 * License:           GPL-2.0-or-later
 * License URI:       https://www.gnu.org/licenses/gpl-2.0.html
 * Text Domain:       datamachine-agent-ci-driver
 *
 * This plugin intentionally has no runtime behavior. Its purpose is to give
 * Data Machine agent CI runs a stable plugin path:
 *   /wordpress/wp-content/plugins/datamachine-agent-ci-driver/
 *
 * The Homeboy WP Codebox agent-task executor mounts this file into WP Codebox
 * via wp_codebox_mounts so consumers do
 * not have to ship their own CI driver plugin.
 */

defined( 'ABSPATH' ) || exit;
