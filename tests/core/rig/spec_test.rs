//! Spec parsing tests — serde round-trips, pipeline step discriminants,
//! service-kind parsing. Covers `src/core/rig/spec.rs`.

use crate::core::rig::{
    PipelineStep, RigResourcesSpec, RigSpec, ServiceKind, ServiceSpec, SharedPathSpec, SymlinkSpec,
    TraceVariantSpec, WorkloadSpec,
};

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
    "shared_paths": [
        {
            "link": "${components.studio.path}/node_modules",
            "target": "~/Developer/studio/node_modules"
        }
    ],
    "resources": {
        "exclusive": ["studio-runtime"],
        "paths": ["~/Developer/studio@bfb-mu-plugin-agent-output"],
        "ports": [9724],
        "process_patterns": ["wordpress-server-child.mjs"]
    },
    "pipeline": {
        "up": [
            { "kind": "service", "id": "tarball-server", "op": "start" },
            { "kind": "symlink", "op": "ensure" },
            { "kind": "shared-path", "op": "ensure" }
        ],
        "check": [
            { "kind": "service", "id": "tarball-server", "op": "health" },
            { "kind": "symlink", "op": "verify" },
            { "kind": "shared-path", "op": "verify" },
            {
                "kind": "check",
                "label": "MDI db.php drop-in survived",
                "file": "~/Studio/intelligence-chubes4/wp-content/db.php",
                "contains": "Markdown Database Integration"
            }
        ],
        "down": [
            { "kind": "shared-path", "op": "cleanup" },
            { "kind": "service", "id": "tarball-server", "op": "stop" }
        ]
    }
}"#;

#[test]
fn test_spec_parses_studio_playground_fixture() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    assert_eq!(spec.id, "studio-playground-dev");
    assert_eq!(spec.components.len(), 2);
    assert_eq!(spec.services.len(), 1);
    assert_eq!(spec.symlinks.len(), 1);
    assert_eq!(spec.shared_paths.len(), 1);
    assert_eq!(spec.resources.exclusive, vec!["studio-runtime"]);
    assert_eq!(spec.pipeline.get("up").unwrap().len(), 3);
    assert_eq!(spec.pipeline.get("check").unwrap().len(), 4);
    assert_eq!(spec.pipeline.get("down").unwrap().len(), 2);
}

#[test]
fn test_spec_http_static_service_kind_roundtrips() {
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
fn test_spec_pipeline_steps_discriminate_correctly() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let up = spec.pipeline.get("up").unwrap();
    assert!(matches!(up[0], PipelineStep::Service { .. }));
    assert!(matches!(up[1], PipelineStep::Symlink { .. }));
    assert!(matches!(up[2], PipelineStep::SharedPath { .. }));

    let check = spec.pipeline.get("check").unwrap();
    assert!(matches!(check[3], PipelineStep::Check { .. }));

    let declarative_spec: RigSpec = serde_json::from_str(
        r#"{
            "id": "declarative-primitives",
            "pipeline": {
                "check": [
                    {
                        "kind": "check",
                        "label": "compat server exists",
                        "any_file_exists": ["lib/compat/runtime-v2/server.txt", "lib/compat/runtime-v1/server.txt"]
                    }
                ],
                "bench_prepare": [
                    {
                        "kind": "command-if-missing",
                        "label": "Install dependencies when project-env is missing",
                        "cwd": "/tmp/project",
                        "missing": "node_modules/.bin/project-env",
                        "command": "npm install"
                    }
                ]
            }
        }"#,
    )
    .expect("parse declarative primitives");

    let declarative_check = declarative_spec
        .pipeline
        .get("check")
        .expect("check pipeline");
    match &declarative_check[0] {
        PipelineStep::Check { spec, .. } => assert_eq!(spec.any_file_exists.len(), 2),
        other => panic!("expected check step, got {other:?}"),
    }

    let prepare = declarative_spec
        .pipeline
        .get("bench_prepare")
        .expect("prepare pipeline");
    match &prepare[0] {
        PipelineStep::CommandIfMissing {
            missing, cmd, cwd, ..
        } => {
            assert_eq!(missing, "node_modules/.bin/project-env");
            assert_eq!(cmd, "npm install");
            assert_eq!(cwd.as_deref(), Some("/tmp/project"));
        }
        other => panic!("expected command-if-missing step, got {other:?}"),
    }
}

#[test]
fn test_spec_symlink_fields_parse() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let link: &SymlinkSpec = &spec.symlinks[0];
    assert_eq!(link.link, "~/.local/bin/studio");
    assert_eq!(link.target, "~/.local/bin/studio-dev");
}

#[test]
fn test_spec_shared_path_fields_parse() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let shared: &SharedPathSpec = &spec.shared_paths[0];
    assert_eq!(shared.link, "${components.studio.path}/node_modules");
    assert_eq!(shared.target, "~/Developer/studio/node_modules");
}

#[test]
fn test_spec_minimal_only_required_fields() {
    let json = r#"{"id": "tiny"}"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    assert_eq!(spec.id, "tiny");
    assert!(spec.components.is_empty());
    assert!(spec.services.is_empty());
    assert!(spec.symlinks.is_empty());
    assert!(spec.shared_paths.is_empty());
    assert!(spec.resources.is_empty());
    assert!(spec.pipeline.is_empty());
}

#[test]
fn test_spec_trace_variants_parse_multi_component_overlays() {
    let json = r#"{
        "id": "studio-playground-dev",
        "components": {
            "studio": { "path": "/tmp/studio" },
            "wordpress-playground": { "path": "/tmp/playground" }
        },
        "trace_variants": {
            "fast-create-site": {
                "overlays": [
                    { "component": "studio", "overlay": "overlays/studio.patch" },
                    { "component": "wordpress-playground", "overlay": "overlays/playground.patch" }
                ]
            }
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let variant = spec
        .trace_variants
        .get("fast-create-site")
        .expect("variant");

    assert_eq!(variant.overlays.len(), 2);
    assert_eq!(variant.overlays[0].component, "studio");
    assert_eq!(variant.overlays[0].overlay, "overlays/studio.patch");
    assert_eq!(variant.overlays[1].component, "wordpress-playground");
    assert_eq!(variant.overlays[1].overlay, "overlays/playground.patch");
}

#[test]
fn test_spec_resources_block_parses_full_shape() {
    let json = r#"{
        "id": "studio-bfb",
        "resources": {
            "exclusive": ["studio-runtime"],
            "paths": ["~/Developer/studio@bfb-mu-plugin-agent-output"],
            "ports": [9724],
            "process_patterns": ["wordpress-server-child.mjs"]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    assert_eq!(spec.resources.exclusive, vec!["studio-runtime"]);
    assert_eq!(
        spec.resources.paths,
        vec!["~/Developer/studio@bfb-mu-plugin-agent-output"]
    );
    assert_eq!(spec.resources.ports, vec![9724]);
    assert_eq!(
        spec.resources.process_patterns,
        vec!["wordpress-server-child.mjs"]
    );
}

#[test]
fn test_spec_resources_defaults_and_serializes_away_when_missing() {
    let spec: RigSpec = serde_json::from_str(r#"{"id":"tiny"}"#).expect("parse");
    assert_eq!(spec.resources, RigResourcesSpec::default());
    let json = serde_json::to_string(&spec).expect("serialize");
    assert!(!json.contains("resources"));
}

#[test]
fn test_spec_resources_rejects_invalid_port_shape() {
    let json = r#"{"id":"bad","resources":{"ports":[70000]}}"#;
    let err = serde_json::from_str::<RigSpec>(json).expect_err("u16 port rejected");
    assert!(err.to_string().contains("70000"));
}

#[test]
fn test_spec_command_service_kind() {
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
fn test_spec_check_step_with_command_probe() {
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
        PipelineStep::Check { label, spec, .. } => {
            assert_eq!(label.as_deref(), Some("docker daemon running"));
            assert_eq!(spec.command.as_deref(), Some("docker info"));
            assert_eq!(spec.expect_exit, Some(0));
        }
        other => panic!("expected Check, got {:?}", other),
    }
}

#[test]
fn test_spec_check_step_parses_groups() {
    let json = r#"{
        "id": "r",
        "pipeline": {
            "check": [
                {
                    "kind": "check",
                    "label": "desktop app packaged",
                    "groups": ["desktop-app", "trace"],
                    "command": "test -d apps/studio/out"
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let steps = spec.pipeline.get("check").unwrap();
    match &steps[0] {
        PipelineStep::Check { groups, .. } => {
            assert_eq!(
                groups,
                &vec!["desktop-app".to_string(), "trace".to_string()]
            );
        }
        other => panic!("expected Check, got {:?}", other),
    }
}

#[test]
fn test_check_groups() {
    let unscoped: WorkloadSpec = serde_json::from_str(r#"{ "path": "/tmp/unscoped.trace.mjs" }"#)
        .expect("parse unscoped workload");
    assert_eq!(unscoped.path(), "/tmp/unscoped.trace.mjs");
    assert!(unscoped.check_groups().is_none());

    let detailed: WorkloadSpec = serde_json::from_str(
        r#"{
            "path": "/tmp/scoped.trace.mjs",
            "check_groups": ["desktop-app"]
        }"#,
    )
    .expect("parse detailed workload");
    assert_eq!(detailed.path(), "/tmp/scoped.trace.mjs");
    assert_eq!(
        detailed.check_groups(),
        Some(&["desktop-app".to_string()][..])
    );
}

#[test]
fn test_trace_phase_preset() {
    let workload = workload_with_trace_metadata();

    assert_eq!(
        workload.trace_phase_preset("startup"),
        Some(&["boot:runner.boot".to_string()][..])
    );
    assert!(workload.trace_phase_preset("missing").is_none());
}

#[test]
fn test_trace_default_phase_preset() {
    let workload = workload_with_trace_metadata();

    assert_eq!(workload.trace_default_phase_preset(), Some("startup"));
    let workload_without_default: WorkloadSpec =
        serde_json::from_str(r#"{ "path": "/tmp/no-default.trace.mjs" }"#).expect("parse workload");
    assert!(workload_without_default
        .trace_default_phase_preset()
        .is_none());
}

#[test]
fn test_trace_variants() {
    let workload = workload_with_trace_metadata();
    let variants = workload.trace_variants();

    assert_eq!(
        variants.get("fresh-install-mode"),
        Some(&TraceVariantSpec {
            component: Some("studio".to_string()),
            overlay: Some("overlays/fresh-install-mode.patch".to_string()),
            overlays: Vec::new(),
            trace_guardrails: Vec::new(),
        })
    );
    let workload_without_variants: WorkloadSpec =
        serde_json::from_str(r#"{ "path": "/tmp/no-variants.trace.mjs" }"#)
            .expect("parse workload");
    assert!(workload_without_variants.trace_variants().is_empty());
}

fn workload_with_trace_metadata() -> WorkloadSpec {
    WorkloadSpec {
        path: "/tmp/scoped.trace.mjs".to_string(),
        check_groups: Some(vec!["desktop-app".to_string()]),
        port_range_size: None,
        named_leases: Vec::new(),
        trace_phase_presets: std::collections::HashMap::from([(
            "startup".to_string(),
            vec!["boot:runner.boot".to_string()],
        )]),
        trace_span_metadata: std::collections::HashMap::new(),
        trace_default_phase_preset: Some("startup".to_string()),
        trace_variants: std::collections::HashMap::from([(
            "fresh-install-mode".to_string(),
            TraceVariantSpec {
                component: Some("studio".to_string()),
                overlay: Some("overlays/fresh-install-mode.patch".to_string()),
                overlays: Vec::new(),
                trace_guardrails: Vec::new(),
            },
        )]),
        trace_guardrails: Vec::new(),
        trace_probes: Vec::new(),
    }
}

#[test]
fn test_spec_build_step_parses() {
    let json = r#"{
        "id": "r",
        "components": { "studio": { "path": "/tmp/studio" } },
        "pipeline": {
            "up": [
                { "kind": "build", "component": "studio", "label": "compile studio" }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let steps = spec.pipeline.get("up").unwrap();
    match &steps[0] {
        PipelineStep::Build {
            component, label, ..
        } => {
            assert_eq!(component, "studio");
            assert_eq!(label.as_deref(), Some("compile studio"));
        }
        other => panic!("expected Build, got {:?}", other),
    }
}

#[test]
fn test_spec_extension_step_parses() {
    let json = r#"{
        "id": "r",
        "components": { "studio": { "path": "/tmp/studio" } },
        "pipeline": {
            "up": [
                { "kind": "extension", "component": "studio", "op": "build", "label": "extension build" }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let steps = spec.pipeline.get("up").unwrap();
    match &steps[0] {
        PipelineStep::Extension {
            component,
            op,
            label,
            ..
        } => {
            assert_eq!(component, "studio");
            assert_eq!(op, "build");
            assert_eq!(label.as_deref(), Some("extension build"));
        }
        other => panic!("expected Extension, got {:?}", other),
    }
}

#[test]
fn test_spec_pipeline_step_id_and_dependencies_parse() {
    let json = r#"{
        "id": "r",
        "components": {
            "studio": { "path": "/tmp/studio" },
            "playground": { "path": "/tmp/playground" }
        },
        "pipeline": {
            "up": [
                { "kind": "build", "id": "playground-build", "component": "playground" },
                {
                    "kind": "build",
                    "id": "studio-install",
                    "component": "studio",
                    "depends_on": ["playground-build"]
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let steps = spec.pipeline.get("up").unwrap();
    match &steps[1] {
        PipelineStep::Build {
            step_id,
            depends_on,
            component,
            ..
        } => {
            assert_eq!(step_id.as_deref(), Some("studio-install"));
            assert_eq!(depends_on, &vec!["playground-build".to_string()]);
            assert_eq!(component, "studio");
        }
        other => panic!("expected Build, got {:?}", other),
    }
}

#[test]
fn test_spec_git_step_parses_with_args() {
    use crate::core::rig::spec::GitOp;
    let json = r#"{
        "id": "r",
        "components": { "studio": { "path": "/tmp/studio" } },
        "pipeline": {
            "sync": [
                {
                    "kind": "git",
                    "component": "studio",
                    "op": "pull",
                    "args": ["origin", "trunk"]
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let steps = spec.pipeline.get("sync").unwrap();
    match &steps[0] {
        PipelineStep::Git {
            component,
            op,
            args,
            ..
        } => {
            assert_eq!(component, "studio");
            assert_eq!(*op, GitOp::Pull);
            assert_eq!(args, &vec!["origin".to_string(), "trunk".to_string()]);
        }
        other => panic!("expected Git, got {:?}", other),
    }
}

#[test]
fn test_spec_git_op_current_branch_kebab_serializes() {
    use crate::core::rig::spec::GitOp;
    let json = r#"{
        "id": "r",
        "components": { "studio": { "path": "/tmp/studio" } },
        "pipeline": {
            "check": [
                { "kind": "git", "component": "studio", "op": "current-branch" }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    match &spec.pipeline.get("check").unwrap()[0] {
        PipelineStep::Git { op, .. } => assert_eq!(*op, GitOp::CurrentBranch),
        other => panic!("expected Git, got {:?}", other),
    }
}

#[test]
fn test_spec_stack_step_parses_sync_shape() {
    use crate::core::rig::spec::StackOp;
    let json = r#"{
        "id": "r",
        "components": {
            "studio": {
                "path": "/tmp/studio",
                "branch": "dev/combined-fixes",
                "stack": "studio-combined"
            }
        },
        "pipeline": {
            "sync": [
                {
                    "kind": "stack",
                    "id": "sync-studio-stack",
                    "component": "studio",
                    "op": "sync",
                    "dry_run": true,
                    "label": "sync Studio combined fixes"
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    assert_eq!(
        spec.components.get("studio").unwrap().stack.as_deref(),
        Some("studio-combined")
    );
    match &spec.pipeline.get("sync").unwrap()[0] {
        PipelineStep::Stack {
            step_id,
            component,
            op,
            dry_run,
            label,
            ..
        } => {
            assert_eq!(step_id.as_deref(), Some("sync-studio-stack"));
            assert_eq!(component, "studio");
            assert_eq!(*op, StackOp::Sync);
            assert!(*dry_run);
            assert_eq!(label.as_deref(), Some("sync Studio combined fixes"));
        }
        other => panic!("expected Stack, got {:?}", other),
    }
}

#[test]
fn test_spec_round_trip_preserves_shape() {
    let spec: RigSpec = serde_json::from_str(STUDIO_PLAYGROUND_SPEC).expect("parse");
    let re_serialized = serde_json::to_string(&spec).expect("serialize");
    let re_parsed: RigSpec = serde_json::from_str(&re_serialized).expect("reparse");
    assert_eq!(re_parsed.id, spec.id);
    assert_eq!(re_parsed.services.len(), spec.services.len());
    assert_eq!(re_parsed.pipeline.len(), spec.pipeline.len());
}

#[test]
fn test_spec_patch_step_parses_full_shape() {
    use crate::core::rig::spec::PatchOp;
    let json = r#"{
        "id": "r",
        "components": { "playground": { "path": "/tmp/pg" } },
        "pipeline": {
            "up": [
                {
                    "kind": "patch",
                    "component": "playground",
                    "file": "packages/php-wasm/compile/dns_polyfill.c",
                    "marker": "PHP-WASM-COMBINED-FIXES TSRMLS fallback",
                    "after": "/* existing fallback */",
                    "content": "/* PHP-WASM-COMBINED-FIXES TSRMLS fallback */\n#define TSRMLS_CC\n",
                    "op": "apply",
                    "label": "TSRMLS fallback"
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    match &spec.pipeline.get("up").unwrap()[0] {
        PipelineStep::Patch {
            component,
            file,
            marker,
            after,
            content,
            op,
            label,
            ..
        } => {
            assert_eq!(component, "playground");
            assert_eq!(file, "packages/php-wasm/compile/dns_polyfill.c");
            assert!(marker.contains("TSRMLS"));
            assert!(after.is_some());
            assert!(content.contains("TSRMLS_CC"));
            assert_eq!(*op, PatchOp::Apply);
            assert_eq!(label.as_deref(), Some("TSRMLS fallback"));
        }
        other => panic!("expected Patch, got {:?}", other),
    }
}

#[test]
fn test_spec_patch_op_defaults_to_apply_when_omitted() {
    use crate::core::rig::spec::PatchOp;
    let json = r#"{
        "id": "r",
        "components": { "c": { "path": "/tmp/c" } },
        "pipeline": {
            "up": [
                {
                    "kind": "patch",
                    "component": "c",
                    "file": "x.c",
                    "marker": "M",
                    "content": "M\n"
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    match &spec.pipeline.get("up").unwrap()[0] {
        PipelineStep::Patch { op, .. } => assert_eq!(*op, PatchOp::Apply),
        other => panic!("expected Patch, got {:?}", other),
    }
}

#[test]
fn test_spec_external_service_kind_with_discover() {
    let json = r#"{
        "id": "r",
        "services": {
            "studio-daemon": {
                "kind": "external",
                "discover": {
                    "pattern": "wordpress-server-child.mjs",
                    "argv_contains": ["studio@bfb-mu-plugin", "playground-server-child.mjs"]
                }
            }
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let svc = spec.services.get("studio-daemon").unwrap();
    assert_eq!(svc.kind, ServiceKind::External);
    assert_eq!(
        svc.discover.as_ref().unwrap().pattern,
        "wordpress-server-child.mjs"
    );
    assert_eq!(
        svc.discover.as_ref().unwrap().argv_contains,
        vec![
            "studio@bfb-mu-plugin".to_string(),
            "playground-server-child.mjs".to_string()
        ]
    );
}

#[test]
fn test_spec_newer_than_check_parses() {
    let json = r#"{
        "id": "r",
        "components": { "studio": { "path": "/tmp/studio" } },
        "pipeline": {
            "check": [
                {
                    "kind": "check",
                    "label": "Daemon newer than bundle",
                    "newer_than": {
                        "left":  { "process_start": { "pattern": "wordpress-server-child.mjs" } },
                        "right": { "file_mtime": "${components.studio.path}/apps/cli/dist/cli/main.mjs" }
                    }
                }
            ]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    match &spec.pipeline.get("check").unwrap()[0] {
        PipelineStep::Check { spec, .. } => {
            let nt = spec.newer_than.as_ref().expect("newer_than present");
            assert_eq!(
                nt.left.process_start.as_ref().unwrap().pattern,
                "wordpress-server-child.mjs"
            );
            assert!(nt
                .right
                .file_mtime
                .as_ref()
                .unwrap()
                .contains("dist/cli/main.mjs"));
        }
        other => panic!("expected Check, got {:?}", other),
    }
}
