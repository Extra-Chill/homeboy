<?php
/**
 * Noop bench workload — measures the per-iteration overhead of the bench
 * harness itself.
 *
 * Returns a callable that does nothing. The recorded p50 is therefore the
 * bench harness's own iteration overhead (hrtime + array push + closure
 * invoke). Useful as a calibration scenario and a self-test that the
 * dispatcher round-trips end-to-end.
 */
return function (): array {
    return ['kind' => 'noop'];
};
