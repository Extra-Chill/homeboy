//! Check evaluator tests for `src/core/rig/check.rs`.
//!
//! HTTP checks require a reachable endpoint which is fragile in CI; the
//! `file` and `command` probes exercise the full one-of-three logic,
//! short-circuit on validation errors, and cover substring matching.

use crate::check::{evaluate, evaluate_with_http_wait_ready_budget};
use crate::spec::{CheckSpec, RigSpec};

fn set_modified(path: &std::path::Path, seconds_since_epoch: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open file for mtime update")
        .set_times(std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds_since_epoch),
        ))
        .expect("set file mtime");
}

fn minimal_rig() -> RigSpec {
    RigSpec {
        effective_component_paths: Default::default(),
        workloads: Default::default(),
        id: "t".to_string(),
        description: String::new(),
        components: Default::default(),
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        lifecycle: Default::default(),
        requirements: Default::default(),
        pipeline: Default::default(),
        bench: None,
        fuzz: None,
        trace: Default::default(),
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        fuzz_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
        fuzz_profiles: Default::default(),
        app_launcher: None,
    }
}

/// Evaluate each `(label, spec, expected_error)` case: `None` must pass,
/// `Some(fragment)` must fail with a message containing `fragment`.
fn assert_cases(cases: Vec<(&str, CheckSpec, Option<&str>)>) {
    let rig = minimal_rig();
    for (label, spec, expected_error) in cases {
        match expected_error {
            None => evaluate(&rig, &spec).unwrap_or_else(|e| panic!("{label}: {}", e.message)),
            Some(fragment) => {
                let err = evaluate(&rig, &spec).expect_err(label);
                assert!(
                    err.message.contains(fragment),
                    "{label}: expected `{fragment}` in: {}",
                    err.message
                );
            }
        }
    }
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn test_evaluate_rejects_invalid_probe_specs() {
    use crate::spec::{NewerThanSpec, TimeSource};
    assert_cases(vec![
        // The empty-spec error names every probe so users discover `newer_than`.
        ("empty spec", CheckSpec::default(), Some("newer_than")),
        (
            "multiple probes",
            CheckSpec {
                http: Some("http://example.com".to_string()),
                any_file_exists: vec!["/tmp/x".to_string()],
                ..Default::default()
            },
            Some("must specify exactly one of"),
        ),
        (
            "empty newer_than time source",
            CheckSpec {
                newer_than: Some(NewerThanSpec {
                    left: TimeSource::default(),
                    right: TimeSource::default(),
                }),
                ..Default::default()
            },
            Some("must specify one of"),
        ),
    ]);
}

#[test]
fn test_evaluate_file_probes() {
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let existing = tmp_dir.path().join("existing.txt");
    let missing = tmp_dir.path().join("missing.txt");
    std::fs::write(&existing, "hello world\nsecond line\n").expect("write");

    assert_cases(vec![
        (
            "file exists",
            CheckSpec {
                file: Some(path_string(&existing)),
                ..Default::default()
            },
            None,
        ),
        (
            "file missing",
            CheckSpec {
                file: Some(path_string(&missing)),
                ..Default::default()
            },
            Some(""),
        ),
        (
            "file contains substring",
            CheckSpec {
                file: Some(path_string(&existing)),
                contains: Some("world".to_string()),
                ..Default::default()
            },
            None,
        ),
        (
            "file lacks substring",
            CheckSpec {
                file: Some(path_string(&existing)),
                contains: Some("not-in-file".to_string()),
                ..Default::default()
            },
            Some(""),
        ),
        (
            "any candidate exists",
            CheckSpec {
                any_file_exists: vec![path_string(&missing), path_string(&existing)],
                ..Default::default()
            },
            None,
        ),
        (
            "no candidate exists",
            CheckSpec {
                any_file_exists: vec![
                    path_string(&missing),
                    path_string(&tmp_dir.path().join("b.txt")),
                ],
                ..Default::default()
            },
            Some("None of the candidate files exist"),
        ),
    ]);
}

#[test]
fn test_evaluate_command_exit_code() {
    let command = |cmd: &str| CheckSpec {
        command: Some(cmd.to_string()),
        expect_exit: Some(0),
        ..Default::default()
    };
    assert_cases(vec![
        ("`true` exits 0", command("true"), None),
        ("`false` fails expect_exit=0", command("false"), Some("")),
    ]);
}

#[test]
fn test_evaluate_newer_than() {
    use crate::spec::{DiscoverSpec, NewerThanSpec, TimeSource};
    let tmp_dir = tempfile::tempdir().expect("tmpdir");
    let older = tmp_dir.path().join("older.txt");
    let newer = tmp_dir.path().join("newer.txt");
    std::fs::write(&older, "x").expect("write");
    std::fs::write(&newer, "x").expect("write");
    set_modified(&older, 1);
    set_modified(&newer, 2);

    let mtime = |path: &std::path::Path| TimeSource {
        file_mtime: Some(path_string(path)),
        ..Default::default()
    };
    let newer_than = |left: TimeSource, right: TimeSource| CheckSpec {
        newer_than: Some(NewerThanSpec { left, right }),
        ..Default::default()
    };

    assert_cases(vec![
        (
            "newer left passes",
            newer_than(mtime(&newer), mtime(&older)),
            None,
        ),
        (
            "older left fails",
            newer_than(mtime(&older), mtime(&newer)),
            Some("not newer"),
        ),
        // No matching process means there is no stale daemon to flag.
        (
            "absent left process passes",
            newer_than(
                TimeSource {
                    process_start: Some(DiscoverSpec {
                        pattern: "homeboy-test-marker-no-process-matches-this-XQZ-9999".to_string(),
                        argv_contains: Vec::new(),
                    }),
                    ..Default::default()
                },
                mtime(&newer),
            ),
            None,
        ),
    ]);
}

// ─── HTTP wait-ready (Extra-Chill/homeboy#1537) ──────────────────────────────
//
// `service.start` returns once the child is forked, but the kernel may not
// have called `bind()`/`listen()` yet when the next pipeline step
// (`service.health`) fires. `http_check` retries connect-refused failures
// for a bounded budget so single-pass `rig up` doesn't race the listener.
// The tests below pin both the wait-loop semantics and what we
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
    let err =
        evaluate_with_http_wait_ready_budget(&rig, &spec, std::time::Duration::from_millis(200))
            .expect_err("no listener => fail");
    let elapsed = start.elapsed();

    // The injected budget preserves the production retry path without making
    // this test wait out the production ten-second ceiling.
    assert!(
        elapsed >= std::time::Duration::from_millis(180),
        "must exhaust the wait-ready budget; elapsed = {:?}",
        elapsed
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
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
