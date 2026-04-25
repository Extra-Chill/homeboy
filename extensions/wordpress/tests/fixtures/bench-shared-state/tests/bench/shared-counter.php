<?php
/**
 * Shared-counter bench workload — exercises the shared-state contract.
 *
 * Increments a counter file under HOMEBOY_BENCH_SHARED_STATE on every
 * iteration, tagging each line with the instance id. After the run,
 * `cat <shared-state>/counter.log` shows interleaved writes from every
 * iteration of every parallel instance.
 *
 * What this proves end-to-end:
 *
 * - HOMEBOY_BENCH_SHARED_STATE is defined and points at a writable path
 *   that survives across iterations.
 * - HOMEBOY_BENCH_INSTANCE_ID is defined and distinct per parallel
 *   instance (so workloads can attribute writes / contention to a
 *   specific worker).
 * - HOMEBOY_BENCH_CONCURRENCY is defined and matches the --concurrency
 *   homeboy core was invoked with.
 * - File-locking semantics: if two Playground processes hit the same
 *   counter.log concurrently, file_put_contents(... LOCK_EX | FILE_APPEND)
 *   must serialize them without losing writes. Lost writes show up as
 *   missing line numbers in the log — what the MDI #47 / #70 bug class
 *   would surface.
 */
return function (): array {
    $shared = HOMEBOY_BENCH_SHARED_STATE;
    if ($shared === '') {
        // Single-instance / no-shared-state run — no-op.
        return ['kind' => 'shared-counter', 'wrote' => false];
    }

    $instance = HOMEBOY_BENCH_INSTANCE_ID;
    $concurrency = HOMEBOY_BENCH_CONCURRENCY;

    $log_path = $shared . '/counter.log';
    $line = sprintf(
        "%s instance=%d concurrency=%d pid=%d\n",
        microtime(true),
        $instance,
        $concurrency,
        getmypid()
    );
    // LOCK_EX so concurrent instances serialize cleanly. The whole point
    // of the workload is to stress this — drop the lock and you'll see
    // truncated lines under concurrency > 1.
    file_put_contents($log_path, $line, FILE_APPEND | LOCK_EX);

    return [
        'kind' => 'shared-counter',
        'wrote' => true,
        'instance' => $instance,
    ];
};
