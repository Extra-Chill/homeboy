//! Rig stack integration tests for `src/core/rig/stack.rs`.

use std::collections::HashMap;

use crate::core::error::Error;
use crate::core::plan::{HomeboyPlan, PlanKind};
use crate::core::rig::spec::{ComponentSpec, RigSpec};
use crate::core::stack::{GitRef, StackPrEntry, StackSpec, SyncOutput, SyncPreview};

use super::{plan_stack_sync, run_component_sync, run_sync_with, validate_component_stack_path};

fn component(path: &str, stack: Option<&str>) -> ComponentSpec {
    ComponentSpec {
        path: path.to_string(),
        checkout_root: None,
        remote_url: None,
        triage_remote_url: None,
        stack: stack.map(str::to_string),
        branch: None,
        extensions: None,
    }
}

fn rig_with_components(components: HashMap<String, ComponentSpec>) -> RigSpec {
    RigSpec {
        id: "stack-rig".to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        pipeline: Default::default(),
        bench: None,
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
        app_launcher: None,
    }
}

fn sync_output(stack_id: &str, picked: usize, skipped: usize, dropped: usize) -> SyncOutput {
    let would_mutate = picked > 0 || skipped > 0 || dropped > 0;
    SyncOutput {
        preview: SyncPreview {
            plan: stack_sync_plan(stack_id, would_mutate),
            stack_id: stack_id.to_string(),
            component_path: "/tmp/component".to_string(),
            branch: "dev/combined-fixes".to_string(),
            base: "origin/main".to_string(),
            target: "fork/dev/combined-fixes".to_string(),
            dropped: Vec::new(),
            replayed: Vec::new(),
            uncertain: Vec::new(),
            target_exists: true,
            target_ahead: Some(0),
            target_behind: Some(0),
            dropped_count: dropped,
            replayed_count: picked + skipped,
            uncertain_count: 0,
            would_mutate,
            blocked: false,
            success: true,
        },
        applied: Vec::new(),
        dry_run: false,
        picked_count: picked,
        skipped_count: skipped,
        success: true,
    }
}

fn stack_sync_plan(stack_id: &str, would_mutate: bool) -> HomeboyPlan {
    let mut plan = HomeboyPlan::for_description(PlanKind::StackSync, stack_id);
    plan.policy.insert(
        "would_mutate".to_string(),
        serde_json::Value::Bool(would_mutate),
    );
    plan.policy
        .insert("blocked".to_string(), serde_json::Value::Bool(false));
    plan
}

fn stack_spec(id: &str, component_path: &str) -> StackSpec {
    StackSpec {
        id: id.to_string(),
        description: String::new(),
        component: "studio".to_string(),
        component_path: component_path.to_string(),
        base: GitRef {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        },
        target: GitRef {
            remote: "fork".to_string(),
            branch: "dev/combined-fixes".to_string(),
        },
        prs: Vec::<StackPrEntry>::new(),
    }
}

#[test]
fn test_plan_stack_sync_uses_components_with_stack_ids_in_sorted_order() {
    let mut components = HashMap::new();
    components.insert("z".to_string(), component("/tmp/z", Some("z-stack")));
    components.insert("a".to_string(), component("/tmp/a", Some("a-stack")));
    components.insert("plain".to_string(), component("/tmp/plain", None));
    let rig = rig_with_components(components);

    let plan = plan_stack_sync(&rig);

    assert_eq!(plan.len(), 2);
    assert_eq!(plan[0].component_id, "a");
    assert_eq!(plan[0].stack_id, "a-stack");
    assert_eq!(plan[1].component_id, "z");
    assert_eq!(plan[1].stack_id, "z-stack");
}

#[test]
fn test_run_sync_reports_changed_and_noop_statuses() {
    let mut components = HashMap::new();
    components.insert("a".to_string(), component("/tmp/a", Some("a-stack")));
    components.insert("b".to_string(), component("/tmp/b", Some("b-stack")));
    let rig = rig_with_components(components);

    let report = run_sync_with(&rig, false, |component_id, stack_id, dry_run| {
        assert!(!dry_run);
        assert!(component_id == "a" || component_id == "b");
        Ok(match stack_id {
            "a-stack" => sync_output(stack_id, 1, 0, 0),
            "b-stack" => sync_output(stack_id, 0, 0, 0),
            other => panic!("unexpected stack {other}"),
        })
    });

    assert!(report.success);
    assert_eq!(report.stacks.len(), 2);
    assert_eq!(report.stacks[0].status, "changed");
    assert_eq!(report.stacks[0].picked_count, 1);
    assert_eq!(report.stacks[1].status, "no-op");
}

#[test]
fn test_run_sync_stops_on_conflict_before_later_stacks() {
    let mut components = HashMap::new();
    components.insert("a".to_string(), component("/tmp/a", Some("a-stack")));
    components.insert("b".to_string(), component("/tmp/b", Some("b-stack")));
    components.insert("c".to_string(), component("/tmp/c", Some("c-stack")));
    let rig = rig_with_components(components);
    let mut seen = Vec::new();

    let report = run_sync_with(&rig, true, |_component_id, stack_id, dry_run| {
        assert!(dry_run);
        seen.push(stack_id.to_string());
        match stack_id {
            "a-stack" => Ok(sync_output(stack_id, 0, 0, 0)),
            "b-stack" => Err(Error::stack_apply_conflict(
                stack_id,
                123,
                "Extra-Chill/homeboy",
                "conflicting file",
            )),
            other => panic!("unexpected stack {other}"),
        }
    });

    assert!(!report.success);
    assert_eq!(seen, vec!["a-stack".to_string(), "b-stack".to_string()]);
    assert_eq!(report.stacks.len(), 2);
    assert_eq!(report.stacks[0].status, "no-op");
    assert_eq!(report.stacks[1].status, "conflict");
    assert!(report.stacks[1]
        .error
        .as_deref()
        .unwrap()
        .contains("conflicting file"));
}

#[test]
fn test_run_sync_reports_general_failure_status() {
    let mut components = HashMap::new();
    components.insert("a".to_string(), component("/tmp/a", Some("a-stack")));
    let rig = rig_with_components(components);

    let report = run_sync_with(&rig, false, |_component_id, _stack_id, _dry_run| {
        Err(Error::stack_not_found("a-stack", Vec::new()))
    });

    assert!(!report.success);
    assert_eq!(report.stacks.len(), 1);
    assert_eq!(report.stacks[0].status, "failed");
    assert!(report.stacks[0]
        .error
        .as_deref()
        .unwrap()
        .contains("Stack not found"));
}

#[test]
fn test_sync_entry_serializes_counts_and_refs() {
    let mut components = HashMap::new();
    components.insert("a".to_string(), component("/tmp/a", Some("a-stack")));
    let rig = rig_with_components(components);

    let report = run_sync_with(&rig, false, |_component_id, stack_id, _dry_run| {
        Ok(SyncOutput {
            preview: SyncPreview {
                plan: stack_sync_plan(stack_id, true),
                stack_id: stack_id.to_string(),
                component_path: "/tmp/component".to_string(),
                branch: "dev/combined-fixes".to_string(),
                base: GitRef {
                    remote: "origin".to_string(),
                    branch: "main".to_string(),
                }
                .display(),
                target: GitRef {
                    remote: "fork".to_string(),
                    branch: "dev/combined-fixes".to_string(),
                }
                .display(),
                dropped: Vec::new(),
                replayed: Vec::new(),
                uncertain: Vec::new(),
                target_exists: true,
                target_ahead: Some(0),
                target_behind: Some(0),
                dropped_count: 1,
                replayed_count: 3,
                uncertain_count: 0,
                would_mutate: true,
                blocked: false,
                success: true,
            },
            applied: Vec::new(),
            dry_run: false,
            picked_count: 2,
            skipped_count: 1,
            success: true,
        })
    });

    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"component_id\":\"a\""));
    assert!(json.contains("\"stack_id\":\"a-stack\""));
    assert!(json.contains("\"status\":\"changed\""));
    assert!(json.contains("\"picked_count\":2"));
    assert!(json.contains("\"skipped_count\":1"));
    assert!(json.contains("\"dropped_count\":1"));
    assert!(json.contains("\"base\":\"origin/main\""));
    assert!(json.contains("\"target\":\"fork/dev/combined-fixes\""));
}

#[test]
fn test_validate_component_stack_path_accepts_matching_paths() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let component_path = dir.path().to_string_lossy().to_string();
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        component(&component_path, Some("studio-combined")),
    );
    let rig = rig_with_components(components);
    let stack = stack_spec("studio-combined", &component_path);

    validate_component_stack_path(&rig, "studio", &stack).expect("path should match");
}

#[test]
fn test_validate_component_stack_path_accepts_canonical_equivalent_paths() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let nested = dir.path().join("checkout");
    std::fs::create_dir_all(&nested).expect("create checkout");
    let rig_path = nested
        .join("..")
        .join("checkout")
        .to_string_lossy()
        .to_string();
    let stack_path = nested.to_string_lossy().to_string();
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        component(&rig_path, Some("studio-combined")),
    );
    let rig = rig_with_components(components);
    let stack = stack_spec("studio-combined", &stack_path);

    validate_component_stack_path(&rig, "studio", &stack).expect("canonical path should match");
}

#[test]
fn test_validate_component_stack_path_errors_on_mismatch() {
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        component("/tmp/studio", Some("studio-combined")),
    );
    let rig = rig_with_components(components);
    let stack = stack_spec("studio-combined", "/tmp/other-studio");

    let err = validate_component_stack_path(&rig, "studio", &stack).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("paths differ"));
    assert!(msg.contains("/tmp/studio"));
    assert!(msg.contains("/tmp/other-studio"));
}

#[test]
fn test_run_component_sync_errors_when_component_has_no_stack() {
    let mut components = HashMap::new();
    components.insert("plain".to_string(), component("/tmp/plain", None));
    let rig = rig_with_components(components);

    let err = run_component_sync(&rig, "plain", true).unwrap_err();

    assert!(err
        .to_string()
        .contains("component 'plain' does not declare a stack"));
}
