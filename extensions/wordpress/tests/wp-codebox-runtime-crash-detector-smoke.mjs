/**
 * Unit coverage for the WP Codebox fatal-runtime-crash detector (#12617).
 *
 * Acting on a signature only pays off if it is precise: a false positive kills
 * a healthy run, and a false negative costs the full test budget. These pin
 * both edges.
 *
 * External dependencies
 */
import { strict as assert } from 'node:assert';

/**
 * Internal dependencies
 */
import {
  DEFAULT_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS,
  configuredWpCodeboxRuntimeCrashGraceSeconds,
  createWpCodeboxRuntimeCrashDetector,
  detectWpCodeboxRuntimeCrash,
} from '../scripts/lib/wp-codebox-runtime-crash.mjs';

// The signature observed on Extra-Chill/homeboy#12617.
const observed = [
  'Unhandled rejection: RuntimeError: null function or function signature mismatch',
  '    at php.wasm._php_stream_write_filtered (wasm://wasm/php.wasm-05996276:wasm-function[3039]:0x26c31e)',
  '    at php.wasm.mysqlnd_stream_array_from_fd_set (wasm://wasm/php.wasm-05996276:wasm-function[8606]:0x69ad75)',
  '    at php.wasm.zif_mysqli_poll (wasm://wasm/php.wasm-05996276:wasm-function[12986]:0x9949a8)',
].join('\n');

const crash = detectWpCodeboxRuntimeCrash(observed);
assert.equal(crash.id, 'php_wasm_unhandled_rejection');
assert.equal(crash.message, 'RuntimeError: null function or function signature mismatch');
assert.equal(crash.wasm_frame, true);

assert.equal(detectWpCodeboxRuntimeCrash('Uncaught RuntimeError: unreachable').id, 'php_wasm_uncaught_exception');
assert.equal(detectWpCodeboxRuntimeCrash('RuntimeError: memory access out of bounds').id, 'php_wasm_trap');
assert.equal(detectWpCodeboxRuntimeCrash('RuntimeError: table index is out of bounds').id, 'php_wasm_trap');

// Not fatal, and must not be treated as such. A logged stack, a PHP-level
// exception, or the word "RuntimeError" inside ordinary test output all reach
// this detector during a healthy run.
for (const benign of [
  'Running PHPUnit tests via WP Codebox...\nOK (481 tests, 1234 assertions)',
  'PHP Fatal error: Uncaught Error: Call to undefined function foo()',
  '    at php.wasm.execute_ex (wasm://wasm/php.wasm-05996276:wasm-function[17805]:0xb880c4)',
  'There was 1 failure:\n1) RuntimeErrorTest::testHandlesRuntimeError',
  '',
  undefined,
]) {
  assert.equal(detectWpCodeboxRuntimeCrash(benign), null, `expected no crash for: ${String(benign).slice(0, 60)}`);
}

// Streamed output splits wherever the pipe decides to, including mid-signature.
const detector = createWpCodeboxRuntimeCrashDetector();
assert.equal(detector.write('Running the wordpress.run-php setup step...\n'), null);
assert.equal(detector.write('Unhandled reje'), null);
assert.equal(detector.crash(), null);
const streamed = detector.write(Buffer.from('ction: RuntimeError: null function or function signature mismatch\n'));
assert.equal(streamed.id, 'php_wasm_unhandled_rejection');
assert.equal(detector.crash().id, 'php_wasm_unhandled_rejection');

// Only the first crash is reported: a wasm trap cascades, and the first
// signature is the one that explains the run.
assert.equal(detector.write('Unhandled rejection: RuntimeError: unreachable'), null);
assert.equal(detector.crash().message, 'RuntimeError: null function or function signature mismatch');

assert.equal(configuredWpCodeboxRuntimeCrashGraceSeconds({}, {}), DEFAULT_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS);
assert.equal(configuredWpCodeboxRuntimeCrashGraceSeconds({ HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS: '5' }, {}), 5);
assert.equal(configuredWpCodeboxRuntimeCrashGraceSeconds({}, { wp_codebox_runtime_crash_grace_seconds: 30 }), 30);
// Zero is the documented opt-out, not an invalid value.
assert.equal(configuredWpCodeboxRuntimeCrashGraceSeconds({ HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS: '0' }, {}), 0);
assert.throws(() => configuredWpCodeboxRuntimeCrashGraceSeconds({ HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS: '-1' }, {}));
assert.throws(() => configuredWpCodeboxRuntimeCrashGraceSeconds({ HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS: 'soon' }, {}));

console.log('wp-codebox runtime crash detector smoke passed');
