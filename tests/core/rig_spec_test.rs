//! Smoke tests for rig spec parsing + state round-tripping.
//!
//! Kept scope-limited to pure parsing/serialization to avoid touching real
//! `~/.config/homeboy/rigs/` during test runs. End-to-end tests that exercise
//! the service supervisor and `rig up` live in the manual validation flow
//! described in the PR body.

use crate::rig::{PipelineStep, RigSpec, ServiceKind, ServiceSpec, SymlinkSpec};

/// Canonical fixture matching the studio-playground-dev shape used as the
/// first real consumer of the rig primitive.
const STUDIO_PLAYGROUND_SPEC: &str = r#"{
    "id": "studio-playground-dev",
    "description": "Dev Studio + Playground with combined-fixes",
    "components": {
        "studio": { "path": "~/Developer/studio", "branch": "dev/combined-fixes" },
        "wordpress-playground": { "path": "~/Developer/wordpress-playground" }
    },
    "services": {
        "tarball-server": {
            "kind": "http-static",
            "cwd": "${components.wordpress-playground.path}/dist/packages-for-self-hosting",
            "port": 9724,
            "health": { "http": "http://127.0.0.1:9724/", "expect_status": 200 }
        }
    },
    "symlinks": [
        { "link": "~/.local/bin/studio", "target": "~/.local/bin/studio-dev" }
    ],
    "pipeline": {
        "up": [
            { "kind": "service", "id": "tarball-server", "op": "start" },
            { "kind": "symlink", "op": "ensure" }
        ],
        "check": [
            { "kind": "service", "id": "tarball-server", "op": "health" },
            { "kind": "symlink", "op": "verify" },
            {
                "kind": "check",
                "label": "MDI db.php drop-in survived",
                "file": "~/Studio/intelligence-chubes4/wp-content/db.php",
                "contains": "Markdown Database Integration"
            }
        ],
        "down": [
            { "kind": "service", "id": "tarball-server", "op": "stop" }
        ]
    }
}"#;

#[test]
fn parses_studio_playground_spec() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    assert_eq!(spec.id, "studio-playground-dev");
    assert_eq!(spec.components.len(), 2);
    assert_eq!(spec.services.len(), 1);
    assert_eq!(spec.symlinks.len(), 1);
    assert_eq!(spec.pipeline.get("up").unwrap().len(), 2);
    assert_eq!(spec.pipeline.get("check").unwrap().len(), 3);
    assert_eq!(spec.pipeline.get("down").unwrap().len(), 1);
}

#[test]
fn service_spec_http_static_kind_roundtrips() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let svc = spec.services.get("tarball-server").expect("service");
    assert_eq!(svc.kind, ServiceKind::HttpStatic);
    assert_eq!(svc.port, Some(9724));
    assert!(svc.health.is_some());
    let health = svc.health.as_ref().unwrap();
    assert_eq!(health.http.as_deref(), Some("http://127.0.0.1:9724/"));
    assert_eq!(health.expect_status, Some(200));
}

#[test]
fn pipeline_steps_discriminate_correctly() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let up = spec.pipeline.get("up").unwrap();
    matches!(up[0], PipelineStep::Service { .. });
    matches!(up[1], PipelineStep::Symlink { .. });

    let check = spec.pipeline.get("check").unwrap();
    matches!(check[2], PipelineStep::Check { .. });
}

#[test]
fn symlink_spec_fields_parse() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let link: &SymlinkSpec = &spec.symlinks[0];
    assert_eq!(link.link, "~/.local/bin/studio");
    assert_eq!(link.target, "~/.local/bin/studio-dev");
}

#[test]
fn minimal_spec_with_only_required_fields_parses() {
    let json = r#"{"id": "tiny"}"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    assert_eq!(spec.id, "tiny");
    assert!(spec.components.is_empty());
    assert!(spec.services.is_empty());
    assert!(spec.symlinks.is_empty());
    assert!(spec.pipeline.is_empty());
}

#[test]
fn command_service_kind_parses() {
    let json = r#"{
        "id": "r",
        "services": {
            "custom": {
                "kind": "command",
                "command": "redis-server --port 6380"
            }
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let svc: &ServiceSpec = spec.services.get("custom").unwrap();
    assert_eq!(svc.kind, ServiceKind::Command);
    assert_eq!(svc.command.as_deref(), Some("redis-server --port 6380"));
}

#[test]
fn check_step_with_command_kind_parses() {
    let json = r#"{
        "id": "r",
        "pipeline": {
            "check": [
                {
                    "kind": "check",
                    "label": "docker daemon running",
                    "command": "docker info",
                    "expect_exit": 0
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let steps = spec.pipeline.get("check").unwrap();
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        PipelineStep::Check { label, spec } => {
            assert_eq!(label.as_deref(), Some("docker daemon running"));
            assert_eq!(spec.command.as_deref(), Some("docker info"));
            assert_eq!(spec.expect_exit, Some(0));
        }
        other => panic!("expected Check, got {:?}", other),
    }
}

#[test]
fn round_trip_preserves_shape() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    let re_parsed: RigSpec = serde_json::from_str(&re_serialized).expect("reparse");
    assert_eq!(re_parsed.id, spec.id);
    assert_eq!(re_parsed.services.len(), spec.services.len());
    assert_eq!(re_parsed.pipeline.len(), spec.pipeline.len());
}
