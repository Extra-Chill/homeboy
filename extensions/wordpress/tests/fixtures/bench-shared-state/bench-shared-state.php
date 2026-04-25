<?php
/**
 * Plugin Name: Bench Shared-State Fixture
 * Description: Smoke fixture for homeboy bench --shared-state and --concurrency.
 *
 * Has no plugin behaviour beyond loading. The accompanying tests/bench/
 * workloads exercise the shared-state contract (HOMEBOY_BENCH_SHARED_STATE,
 * HOMEBOY_BENCH_INSTANCE_ID, HOMEBOY_BENCH_CONCURRENCY) end-to-end. Pair
 * fixture for bench-noop — same shape, plus the multi-instance contract.
 */
