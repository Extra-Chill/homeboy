//! Check evaluator tests for `src/core/rig/check.rs`.
//!
//! HTTP checks require a reachable endpoint which is fragile in CI; the
//! `file` and `command` probes exercise the full one-of-three logic,
//! short-circuit on validation errors, and cover substring matching.

use crate::rig::check::evaluate;
use crate::rig::spec::{CheckSpec, RigSpec};

fn minimal_rig() -> RigSpec {
    RigSpec {
        id: "t".to_string(),
        description: String::new(),
        components: Default::default(),
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        pipeline: Default::default(),
        bench: None,
        bench_workloads: Default::default(),
        app_launcher: None,
    }
}

#[test]
fn test_evaluate_rejects_empty_spec() {
    let rig = minimal_rig();
    let err = evaluate(&rig, &CheckSpec::default()).expect_err("empty spec rejected");
    assert!(err.message.contains("must specify one of"));
}

#[test]
fn test_evaluate_rejects_multiple_probes() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        http: Some("http://example.com".to_string()),
        file: Some("/tmp/x".to_string()),
        ..Default::default()
    };
    let err = evaluate(&rig, &spec).expect_err("multiple probes rejected");
    assert!(err.message.contains("must specify exactly one of"));
}

#[test]
fn test_evaluate_file_exists() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let rig = minimal_rig();
    let spec = CheckSpec {
        file: Some(tmp.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect("existing file passes");
}

#[test]
fn test_evaluate_file_missing() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        file: Some("/definitely/does/not/exist/ever-420".to_string()),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect_err("missing file fails");
}

#[test]
fn test_evaluate_file_contains_substring() {
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let path = tmp_dir.path().join("check.txt");
    std::fs::write(&path, "hello world\nsecond line\n").expect("write");
    let rig = minimal_rig();

    let pass = CheckSpec {
        file: Some(path.to_string_lossy().into_owned()),
        contains: Some("world".to_string()),
        ..Default::default()
    };
    evaluate(&rig, &pass).expect("substring present");

    let fail = CheckSpec {
        file: Some(path.to_string_lossy().into_owned()),
        contains: Some("not-in-file".to_string()),
        ..Default::default()
    };
    evaluate(&rig, &fail).expect_err("substring absent");
}

#[test]
fn test_evaluate_command_exit_code_matches() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        command: Some("true".to_string()),
        expect_exit: Some(0),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect("`true` exits 0");
}

#[test]
fn test_evaluate_command_unexpected_exit() {
    let rig = minimal_rig();
    let spec = CheckSpec {
        command: Some("false".to_string()),
        expect_exit: Some(0),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect_err("`false` fails expect_exit=0");
}

#[test]
fn test_evaluate_newer_than_left_newer_passes() {
    use crate::rig::spec::{NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let older = tmp_dir.path().join("older.txt");
    let newer = tmp_dir.path().join("newer.txt");
    std::fs::write(&older, "x").expect("write");
    // Sleep a beat so mtimes are distinguishable at second granularity.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&newer, "x").expect("write");

    let rig = minimal_rig();
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource {
                file_mtime: Some(newer.to_string_lossy().into_owned()),
                ..Default::default()
            },
            right: TimeSource {
                file_mtime: Some(older.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    evaluate(&rig, &spec).expect("newer left passes");
}

#[test]
fn test_evaluate_newer_than_left_older_fails() {
    use crate::rig::spec::{NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let older = tmp_dir.path().join("older.txt");
    let newer = tmp_dir.path().join("newer.txt");
    std::fs::write(&older, "x").expect("write");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&newer, "x").expect("write");

    let rig = minimal_rig();
    // left = older, right = newer ⇒ check fails.
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource {
                file_mtime: Some(older.to_string_lossy().into_owned()),
                ..Default::default()
            },
            right: TimeSource {
                file_mtime: Some(newer.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    let err = evaluate(&rig, &spec).expect_err("older left fails");
    assert!(err.message.contains("not newer"));
}

#[test]
fn test_evaluate_newer_than_missing_left_process_passes() {
    use crate::rig::spec::{DiscoverSpec, NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let bundle = tmp_dir.path().join("bundle.js");
    std::fs::write(&bundle, "x").expect("write");

    let rig = minimal_rig();
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource {
                process_start: Some(DiscoverSpec {
                    // Pattern that cannot match any process — ensures None.
                    pattern: "homeboy-test-marker-no-process-matches-this-XQZ-9999".to_string(),
                }),
                ..Default::default()
            },
            right: TimeSource {
                file_mtime: Some(bundle.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }),
        ..Default::default()
    };
    // Left is None ⇒ no stale daemon to flag ⇒ pass.
    evaluate(&rig, &spec).expect("absent left process passes");
}

#[test]
fn test_evaluate_newer_than_rejects_empty_time_source() {
    use crate::rig::spec::{NewerThanSpec, TimeSource};
    let rig = minimal_rig();
    let spec = CheckSpec {
        newer_than: Some(NewerThanSpec {
            left: TimeSource::default(),
            right: TimeSource::default(),
        }),
        ..Default::default()
    };
    let err = evaluate(&rig, &spec).expect_err("empty source rejected");
    assert!(err.message.contains("must specify one of"));
}

#[test]
fn test_evaluate_check_with_no_probe_set_lists_newer_than() {
    let rig = minimal_rig();
    let err = evaluate(&rig, &CheckSpec::default()).expect_err("empty rejected");
    // Documentation drift sentinel — make sure the error names every probe
    // so users see `newer_than` in the suggestion.
    assert!(err.message.contains("newer_than"));
}

// ─── HTTP wait-ready (Extra-Chill/homeboy#1537) ──────────────────────────────
//
// `service.start` returns once the child is forked, but the kernel may not
// have called `bind()`/`listen()` yet when the next pipeline step
// (`service.health`) fires. `http_check` retries connect-refused failures
// for a bounded budget so single-pass `rig up` doesn't race the listener.
// The four tests below pin both the wait-loop semantics and what we
// deliberately do NOT retry on.

/// Reserve a free TCP port by binding then dropping. Used only by tests
/// that want to assert connect-refused behavior on an unbound port. Tests
/// that intend to actually serve a response should use `bind_serving` so
/// the port is held continuously and parallel tests can't steal it
/// between reserve and serve.
fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Bind a listener on an ephemeral port and return both the bound
/// listener and its port. Caller is responsible for accepting on it
/// (typically by passing the listener into `serve_once`). Eliminates
/// the reserve→bind race that plagues `bind ephemeral; drop; rebind`.
fn bind_serving() -> (std::net::TcpListener, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    (listener, port)
}

/// Spawn a one-shot HTTP/1.0 server that answers a single connection
/// with `status` and a tiny body, then exits.
fn serve_once(listener: std::net::TcpListener, status: u16) -> std::thread::JoinHandle<()> {
    use std::io::{Read, Write};
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Drain request just enough that the client doesn't see RST.
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = "ok";
            let response = format!(
                "HTTP/1.0 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    })
}

#[test]
fn test_http_check_retries_until_listener_appears() {
    // Real-world race shape: pipeline runs `service.start` then `service.health`
    // back-to-back. The fix's job is to absorb the bind() gap.
    //
    // We deliberately use `reserve_port` here (not `bind_serving`) so the
    // port is genuinely closed at probe time — that's what reproduces the
    // bind() race the fix targets. The handler thread re-binds after a
    // delay; the retry loop must absorb the gap.
    let port = reserve_port();
    let url = format!("http://127.0.0.1:{}/", port);

    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", port)).expect("rebind listener for race");
        let server = serve_once(listener, 200);
        // Block until the request is served so the test thread's drop
        // doesn't kill the listener mid-handshake.
        let _ = server.join();
    });

    let rig = minimal_rig();
    let spec = CheckSpec {
        http: Some(url),
        expect_status: Some(200),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    evaluate(&rig, &spec).expect("listener came up within budget");
    let elapsed = start.elapsed();

    // Sanity bounds: must have actually waited (>=400ms) and must not
    // have run anywhere near the 10s ceiling.
    assert!(
        elapsed >= std::time::Duration::from_millis(400),
        "expected to wait for the listener; elapsed = {:?}",
        elapsed
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "should converge well before budget exhaustion; elapsed = {:?}",
        elapsed
    );

    let _ = handle.join();
}

#[test]
fn test_http_check_exhausts_budget_when_nothing_ever_listens() {
    // Inverse case: confirm we DO give up. Without a bounded budget this
    // would hang forever; without a budget AT ALL we'd return immediately
    // and re-introduce the original race.
    let port = reserve_port();
    let url = format!("http://127.0.0.1:{}/", port);

    let rig = minimal_rig();
    let spec = CheckSpec {
        http: Some(url),
        expect_status: Some(200),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let err = evaluate(&rig, &spec).expect_err("no listener => fail");
    let elapsed = start.elapsed();

    // Budget is 10s; allow generous slack for slow CI but cap below a
    // pathological hang.
    assert!(
        elapsed >= std::time::Duration::from_secs(9),
        "must exhaust the wait-ready budget; elapsed = {:?}",
        elapsed
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "must not hang past the budget; elapsed = {:?}",
        elapsed
    );
    // Final error should still surface as a connect failure, not a fake
    // success or a confusing fallback.
    assert!(
        err.message.contains("HTTP GET") && err.message.contains("failed"),
        "expected HTTP failure message, got: {}",
        err.message
    );
}

#[test]
fn test_http_check_passes_when_listener_already_up() {
    // No race in this case — listener is bound before the probe fires.
    // Should return effectively instantly with no retries.
    let (listener, port) = bind_serving();
    let url = format!("http://127.0.0.1:{}/", port);
    let server = serve_once(listener, 200);

    let rig = minimal_rig();
    let spec = CheckSpec {
        http: Some(url),
        expect_status: Some(200),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    evaluate(&rig, &spec).expect("listener up => pass");
    let elapsed = start.elapsed();

    // Should be sub-second; the retry loop must not idle when the very
    // first attempt succeeds.
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "no-race path must be near-instant; elapsed = {:?}",
        elapsed
    );

    let _ = server.join();
}

#[test]
fn test_http_check_does_not_retry_on_unexpected_status() {
    // Critical contract: an HTTP-level response — even a 5xx — means the
    // listener IS up and answering. Retrying on status mismatch would
    // turn `http_check` into an application-level health waiter, which is
    // a different probe (use `command` checks for that). The wait loop is
    // strictly for the bind() race.
    let (listener, port) = bind_serving();
    let url = format!("http://127.0.0.1:{}/", port);
    let server = serve_once(listener, 503); // listener up, wrong status

    let rig = minimal_rig();
    let spec = CheckSpec {
        http: Some(url),
        expect_status: Some(200),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let err = evaluate(&rig, &spec).expect_err("wrong status => fail immediately");
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "status mismatch must not enter the wait loop; elapsed = {:?}",
        elapsed
    );
    assert!(
        err.message.contains("returned 503") && err.message.contains("expected 200"),
        "expected status-mismatch message, got: {}",
        err.message
    );

    let _ = server.join();
}
