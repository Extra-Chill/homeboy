use super::dependency_graph::stack_outdated_for_component;
use super::stack_refresh::{stack_refresh_for_component, DependencyStackEdgeRefreshState};
use super::stack_source::TEST_BASE;
use crate::component::Component;
use std::io::{Read, Write};

fn fixture(responses: Vec<(&'static str, &'static str)>) -> std::thread::JoinHandle<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    TEST_BASE.with(|base| {
        *base.borrow_mut() = Some(format!("http://{}", listener.local_addr().unwrap()))
    });
    std::thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).unwrap();
            assert!(read > 0, "expected an HTTP request");
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    })
}

#[test]
fn http_errors_surface_status_and_github_message() {
    for (url, status, body, expected) in [
        (
            "https://api.github.com/repos/org/tool/releases",
            "403 Forbidden",
            r#"{"message":"API rate limit exceeded"}"#,
            "HTTP 403: API rate limit exceeded",
        ),
        (
            "https://api.github.com/repos/org/tool/releases",
            "502 Bad Gateway",
            "not JSON",
            "HTTP 502: Bad Gateway",
        ),
        (
            "https://registry.npmjs.org/tool",
            "404 Not Found",
            r#"{"message":"not GitHub"}"#,
            "HTTP 404: Not Found",
        ),
    ] {
        let server = fixture(vec![(status, body)]);
        let error = super::stack_source::http_json(url).unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
        server.join().unwrap();
    }
    TEST_BASE.with(|base| *base.borrow_mut() = None);
}

fn consumer(path: &std::path::Path, source: serde_json::Value, policy: bool) -> Component {
    let mut component = Component::new(
        "consumer".into(),
        path.display().to_string(),
        String::new(),
        None,
    );
    let mut edge = serde_json::json!({
        "upstream":"tool", "downstream":"consumer", "package":"tool",
        "source":source, "lock":{"kind":"version-file", "path":"pin"},
        "update":"exit 99", "rebuild":true, "post_update":["exit 99"]
    });
    if policy {
        edge["policy"] = serde_json::json!({"max":"same-major"});
    }
    component.dependency_stack = vec![serde_json::from_value(edge).unwrap()];
    component
}

#[test]
fn version_file_github_refresh_inspects_commits_and_skips_all_commands() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pin"), " 1.2.0 \n").unwrap();
    let component = consumer(
        dir.path(),
        serde_json::json!({"github_release":"org/tool"}),
        false,
    );
    let server = fixture(vec![
        (
            "200 OK",
            r#"[{"tag_name":"v1.2.1","draft":false,"prerelease":false},{"tag_name":"v9.0.0","draft":true,"prerelease":false},{"tag_name":"v8.0.0-beta.1","draft":false,"prerelease":true},{"tag_name":"tool-v7.0.0","draft":false,"prerelease":false}]"#,
        ),
        (
            "200 OK",
            r#"{"total_commits":1,"commits":[{"commit":{"message":"fix!: remove old RPC"}}]}"#,
        ),
    ]);
    let result = stack_refresh_for_component(&component, dir.path(), false, true).unwrap();
    server.join().unwrap();
    assert_eq!(result.updated_count, 1);
    assert_eq!(result.edges[0].breaking, Some(true));
    assert!(result.edges[0].command_results.is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("pin")).unwrap(),
        "1.2.1\n"
    );
}

#[test]
fn github_release_resolution_stops_at_githubs_1000_release_window() {
    // A repo with more than 1000 releases: GitHub returns ten full pages and
    // then 422 for page 11. The fixture serves exactly ten pages, so any
    // eleventh request fails to connect and the edge turns unresolvable.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pin"), "1.0.0\n").unwrap();
    let component = consumer(
        dir.path(),
        serde_json::json!({"github_release":"org/tool"}),
        false,
    );
    let pages: Vec<(&'static str, &'static str)> = (0..10)
        .map(|page| {
            let releases: Vec<String> = (0..100)
                .map(|index| {
                    let patch = 1000 - (page * 100 + index);
                    format!(r#"{{"tag_name":"v1.0.{patch}","draft":false,"prerelease":false}}"#)
                })
                .collect();
            let body: &'static str =
                Box::leak(format!("[{}]", releases.join(",")).into_boxed_str());
            ("200 OK", body)
        })
        .collect();
    let server = fixture(pages);
    let result = stack_outdated_for_component(&component, dir.path()).unwrap();
    server.join().unwrap();
    let edge = serde_json::to_value(&result.edges[0]).unwrap();
    assert_eq!(edge["state"], "behind", "{edge}");
    assert_eq!(edge["newest_version"], "1.0.1000");
}

#[test]
fn version_file_npm_refresh_and_dry_run() {
    for dry_run in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pin"), "0.9.3\n").unwrap();
        let component = consumer(dir.path(), serde_json::json!({"npm":"@org/tool"}), true);
        let server = fixture(vec![("200 OK", r#"{"dist-tags":{"latest":"0.9.4"}}"#)]);
        let result = stack_refresh_for_component(&component, dir.path(), dry_run, true).unwrap();
        server.join().unwrap();
        assert_eq!(result.updated_count, 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pin")).unwrap(),
            if dry_run { "0.9.3\n" } else { "0.9.4\n" }
        );
        assert!(result.edges[0].command_results.is_empty());
    }
}

#[test]
fn breaking_only_policy_does_not_hold_majors_and_npm_ignores_it() {
    for (from, latest, breaking) in [
        ("0.9.3", "0.10.0", Some(true)),
        ("1.0.0", "2.0.0", Some(true)),
        ("1.0.0", "1.0.1", None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pin"), from).unwrap();
        let mut component = consumer(dir.path(), serde_json::json!({"npm":"@org/tool"}), false);
        component.dependency_stack[0].policy =
            Some(serde_json::from_value(serde_json::json!({"breaking":"commit-markers"})).unwrap());
        let body: &'static str =
            Box::leak(format!(r#"{{"dist-tags":{{"latest":"{latest}"}}}}"#).into_boxed_str());
        let server = fixture(vec![("200 OK", body)]);
        let result = stack_refresh_for_component(&component, dir.path(), true, true).unwrap();
        server.join().unwrap();
        assert_eq!(result.updated_count, 1);
        assert_eq!(result.edges[0].breaking, breaking);
    }
}

#[test]
fn version_file_same_major_is_held_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pin"), "0.9.3\n").unwrap();
    let component = consumer(dir.path(), serde_json::json!({"npm":"@org/tool"}), true);
    for refresh in [false, true] {
        let server = fixture(vec![(
            "200 OK",
            r#"{"dist-tags":{"latest":"1.0.0"},"versions":{"0.9.4":{},"1.0.0":{}}}"#,
        )]);
        if refresh {
            let result = stack_refresh_for_component(&component, dir.path(), false, true).unwrap();
            assert_eq!(result.exit_code(), 2);
            assert_eq!(result.edges[0].state, DependencyStackEdgeRefreshState::Held);
            assert!(result.edges[0]
                .unresolvable_reason
                .as_ref()
                .unwrap()
                .contains("new major"));
        } else {
            let result = stack_outdated_for_component(&component, dir.path()).unwrap();
            assert_eq!(result.exit_code(), 2);
            assert_eq!(
                serde_json::to_value(&result.edges[0]).unwrap()["state"],
                "held"
            );
        }
        server.join().unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("pin")).unwrap(),
        "0.9.3\n"
    );
}

#[test]
fn version_file_unreachable_registry_is_never_current() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pin"), "0.9.3\n").unwrap();
    let component = consumer(dir.path(), serde_json::json!({"npm":"@org/tool"}), true);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    TEST_BASE.with(|base| *base.borrow_mut() = Some(url));
    let result = stack_refresh_for_component(&component, dir.path(), false, true).unwrap();
    assert_eq!(result.exit_code(), 2);
    assert_eq!(
        result.edges[0].state,
        DependencyStackEdgeRefreshState::Unresolvable
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("pin")).unwrap(),
        "0.9.3\n"
    );
}

#[test]
fn version_file_invalid_registry_response_is_unresolvable() {
    for body in [r#"{"dist-tags":{"latest":"not-semver"}}"#, "not json"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pin"), "0.9.3\n").unwrap();
        let component = consumer(dir.path(), serde_json::json!({"npm":"tool"}), false);
        let server = fixture(vec![("200 OK", body)]);
        let result = stack_refresh_for_component(&component, dir.path(), false, true).unwrap();
        server.join().unwrap();
        assert_eq!(result.exit_code(), 2);
        assert_eq!(result.current_count, 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pin")).unwrap(),
            "0.9.3\n"
        );
    }
}

#[test]
fn version_file_github_clean_and_incomplete_comparisons() {
    for (comparison, verdict) in [
        (
            r#"{"total_commits":1,"commits":[{"commit":{"message":"fix: compatible"}}]}"#,
            Some(false),
        ),
        (
            r#"{"total_commits":2,"commits":[{"commit":{"message":"fix: compatible"}}]}"#,
            None,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pin"), "1.0.0\n").unwrap();
        let component = consumer(
            dir.path(),
            serde_json::json!({"github_release":"org/tool"}),
            false,
        );
        let server = fixture(vec![
            (
                "200 OK",
                r#"[{"tag_name":"1.0.1","draft":false,"prerelease":false}]"#,
            ),
            ("200 OK", comparison),
        ]);
        let result = stack_refresh_for_component(&component, dir.path(), false, true).unwrap();
        server.join().unwrap();
        assert_eq!(result.updated_count, 1);
        assert_eq!(result.edges[0].breaking, verdict);
    }
}

#[test]
fn version_file_rejects_escaping_paths() {
    let dir = tempfile::tempdir().unwrap();
    assert!(super::stack_source::version_file_path(dir.path(), "../pin").is_err());
    assert!(super::stack_source::version_file_path(dir.path(), "/tmp/pin").is_err());
}
