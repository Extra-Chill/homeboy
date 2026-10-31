#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

docker run --rm \
  -v "${ROOT}/vendor:/wp-codebox-vendor:ro" \
  -v "${ROOT}/scripts/test/wp-cli-phpunit-bootstrap.php:/wp-codebox-wp-cli-bootstrap.php:ro" \
  php:8.3-cli php -r '
require "/wp-codebox-wp-cli-bootstrap.php";
if ( defined( "WP_CLI" ) || ! class_exists( "WP_CLI" ) || ! method_exists( "WP_CLI", "log" ) || ! function_exists( "WP_CLI\\Utils\\format_items" ) ) {
    exit( 1 );
}
echo "WP-CLI PHPUnit bootstrap smoke passed.\n";
'
