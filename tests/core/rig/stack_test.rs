//! Rig stack integration tests for `src/core/rig/stack.rs`.

use std::collections::HashMap;

use crate::error::Error;
use crate::rig::spec::{ComponentSpec, RigSpec};
use crate::stack::{GitRef, SyncOutput, SyncPreview};

use super::{plan_stack_sync, run_sync_with};

fn component(path: &str, stack: Option<&str>) -> ComponentSpec {
    ComponentSpec {
        path: path.to_string(),
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
        app_launcher: None,
    }
}

fn sync_output(stack_id: &str, picked: usize, skipped: usize, dropped: usize) -> SyncOutput {
    SyncOutput {
        preview: SyncPreview {
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
            would_mutate: picked > 0 || dropped > 0,
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

    let report = run_sync_with(&rig, false, |stack_id, dry_run| {
        assert!(!dry_run);
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

    let report = run_sync_with(&rig, true, |stack_id, dry_run| {
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

    let report = run_sync_with(&rig, false, |_stack_id, _dry_run| {
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

    let report = run_sync_with(&rig, false, |stack_id, _dry_run| {
        Ok(SyncOutput {
            preview: SyncPreview {
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
