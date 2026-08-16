/**
 * Detect fatal WP Codebox runtime crashes in streamed recipe-run output.
 *
 * A PHP-WASM trap — `RuntimeError: null function or function signature
 * mismatch` and friends — leaves the interpreter in an undefined state. The WP
 * Codebox CLI logs the rejection instead of exiting, so the recipe-run promise
 * never settles and the runner sits until its budget expires. On #12617 that
 * turned a crash which happened in the first seconds into a 24-minute wall
 * clock, four shards deep, reported only as `test phase timed out ... before
 * reporting test counts`.
 *
 * The signature is an unhandled rejection or uncaught exception carrying a
 * WebAssembly `RuntimeError`. That pairing is what makes it safe to act on: a
 * wasm trap is not recoverable, and an *unhandled* one means nothing in the
 * runtime claimed it. A stack frame mentioning `php.wasm` on its own is not
 * enough — traces get logged for non-fatal reasons — so it only corroborates.
 */

export const WP_CODEBOX_RUNTIME_CRASH_SCHEMA = 'homeboy/wp-codebox-runtime-crash/v1';
export const DEFAULT_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS = 60;

// Retained across chunk boundaries so a signature split by the stream still
// matches. Two lines of slack is enough for every pattern below.
const CARRY_BYTES = 4096;
const MESSAGE_BYTES = 512;

const SIGNATURES = [
  {
    id: 'php_wasm_unhandled_rejection',
    pattern: /Unhandled rejection:\s*(RuntimeError:[^\n]*)/,
  },
  {
    id: 'php_wasm_uncaught_exception',
    pattern: /Uncaught\s+(RuntimeError:[^\n]*)/,
  },
  {
    id: 'php_wasm_trap',
    pattern: /(RuntimeError: (?:null function or function signature mismatch|memory access out of bounds|unreachable|integer divide by zero|table index is out of bounds)[^\n]*)/,
  },
];

const WASM_FRAME = /\bat [\w.]*\.?wasm[\w.$-]*\.|wasm:\/\/wasm\//;

/**
 * Inspect a complete text buffer for a fatal runtime crash.
 *
 * @param {string} text
 * @return {{ id: string, message: string, wasm_frame: boolean } | null} The
 *   first matching crash, or null when the text carries no fatal signature.
 */
export function detectWpCodeboxRuntimeCrash(text) {
  const haystack = typeof text === 'string' ? text : '';
  if (!haystack) {
    return null;
  }
  for (const signature of SIGNATURES) {
    const match = signature.pattern.exec(haystack);
    if (!match) {
      continue;
    }
    return {
      id: signature.id,
      message: boundedMessage(match[1] || match[0]),
      wasm_frame: WASM_FRAME.test(haystack),
    };
  }
  return null;
}

/**
 * Stateful detector for chunked output.
 *
 * Reports the first crash only: a wasm trap usually cascades, and the first
 * signature is the one that explains the run.
 *
 * @return {{ write: (chunk: Buffer | string) => Object | null, crash: () => Object | null }} A
 *   detector holding the first crash it sees.
 */
export function createWpCodeboxRuntimeCrashDetector() {
  let carry = '';
  let crash = null;

  return {
    write(chunk) {
      if (crash) {
        return null;
      }
      const text = `${carry}${Buffer.isBuffer(chunk) ? chunk.toString('utf8') : String(chunk ?? '')}`;
      const detected = detectWpCodeboxRuntimeCrash(text);
      if (detected) {
        crash = detected;
        carry = '';
        return crash;
      }
      carry = text.length > CARRY_BYTES ? text.slice(-CARRY_BYTES) : text;
      return null;
    },
    crash() {
      return crash;
    },
  };
}

/**
 * Seconds to wait after a crash signature before terminating the runtime.
 *
 * A grace window is what makes acting on the signature safe: a run that somehow
 * recovers and finishes inside the window is untouched, while a wedged one is
 * cut from the full budget to about a minute. `0` disables early termination.
 *
 * @param {NodeJS.ProcessEnv} [environment]
 * @param {Object}            [settings]
 * @return {number} Grace period in seconds.
 */
export function configuredWpCodeboxRuntimeCrashGraceSeconds(environment = process.env, settings = {}) {
  const raw = environment.HOMEBOY_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS
    ?? settings.wp_codebox_runtime_crash_grace_seconds
    ?? DEFAULT_WP_CODEBOX_RUNTIME_CRASH_GRACE_SECONDS;
  const seconds = Number(raw);
  if (!Number.isSafeInteger(seconds) || seconds < 0) {
    throw new Error('WP Codebox runtime crash grace must be a non-negative integer number of seconds.');
  }
  return seconds;
}

function boundedMessage(value) {
  const text = String(value ?? '').trim().replace(/\s+/g, ' ');
  return Buffer.byteLength(text) > MESSAGE_BYTES
    ? Buffer.from(text).subarray(0, MESSAGE_BYTES).toString('utf8')
    : text;
}
