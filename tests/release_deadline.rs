use std::process::Command;

/// `HOMEBOY_RELEASE_DEADLINE_SECS` must abort a slow release command and emit a
/// structured timeout envelope instead of hanging or dying silently.
///
/// Ignored because it cannot decide anything reliably, and it was deciding
/// whether releases ship (#14880).
///
/// It is non-deterministic in both directions, for the same underlying reason:
/// it races real work against a two-second deadline and asserts on the result.
///
/// * Too slow — the original form also required the whole command to finish
///   inside 8 seconds. That measures the runner, not Homeboy. On a contended
///   GitHub-hosted runner the same unchanged code passed or failed depending on
///   what else was executing. Two consecutive releases failed this way on
///   2026-09-22, holding merged work for hours.
/// * Too fast — on a developer machine with warm local config the command
///   completes well inside two seconds, no timeout fires, and the run exits 0:
///
///   ```text
///   assertion `left == right` failed
///     left: Some(0)
///    right: Some(124)
///   ```
///
/// So the pass condition is "the host is slow, but not too slow", which is a
/// property of the machine rather than of the code under test. Because this
/// file runs inside `homeboy review test`, which gates releases, that property
/// was gating releases.
///
/// The behaviour it targets is real and worth covering. Doing so needs a
/// deterministic slow path — a command that exceeds the deadline by
/// construction rather than by ambient load — instead of racing live component
/// resolution. That is tracked in #14880; this stays ignored until then so a
/// stopwatch cannot block a release train.
///
/// Run deliberately with: `cargo test --test release_deadline -- --ignored`
#[test]
#[ignore = "non-deterministic: races real work against a 2s deadline; see #14880"]
fn release_version_show_emits_a_structured_timeout_envelope() {
    let output = Command::new(env!("CARGO_BIN_EXE_homeboy"))
        .args(["release", "version", "show"])
        .env("HOMEBOY_RELEASE_DEADLINE_SECS", "2")
        .output()
        .expect("run release version show");

    assert_eq!(output.status.code(), Some(124));
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("timeout must emit JSON envelope");
    assert_eq!(envelope["schema"], "homeboy/command-result/v3");
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["exit_code"], 124);
    assert!(envelope["diagnostics"]["details"]["error"]
        .as_str()
        .expect("timeout cause")
        .contains("version show: resolving component"));
}
