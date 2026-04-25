<?php
/**
 * Plugin Name: Bench Env Fixture
 * Description: Smoke fixture for the bench_env setting end-to-end. Components declare host-shell env vars to forward into Playground PHP-WASM under extensions.wordpress.settings.bench_env in their homeboy.json; the dispatcher merges them via HOMEBOY_SETTINGS_JSON and the runner putenv()'s each entry before fixtures load.
 *
 * The accompanying tests/bench workload reads back the env var via
 * getenv() and writes its value to a shared-state log so the smoke
 * script can grep for it outside Playground.
 *
 * Pair fixture for bench-noop / bench-shared-state / wp-config-defines.
 */
