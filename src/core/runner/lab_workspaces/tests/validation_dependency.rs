#![cfg(test)]

use super::super::workspace_mapping_entry_for_validation_dependency;
use crate::core::runner::RunnerValidationDependencySyncOutput;

fn dependency_output() -> RunnerValidationDependencySyncOutput {
    RunnerValidationDependencySyncOutput {
        id: "shared-runtime".to_string(),
        role: "validation_dependency".to_string(),
        local_path: "/Users/dev/Developer/shared-runtime".to_string(),
        remote_path: "/srv/_lab_workspaces/shared-runtime".to_string(),
        evidence_path: "/srv/_lab_workspaces/shared-runtime/.homeboy/lab-source-evidence.json"
            .to_string(),
    }
}

#[test]
fn maps_dependency_local_path_to_materialized_remote_path() {
    let entry = workspace_mapping_entry_for_validation_dependency(&dependency_output());

    // The controller-local checkout path must remap to the runner-side
    // materialized path so remote dependency resolvers receive usable paths
    // instead of controller-local ones (#3292).
    assert_eq!(entry.local_path(), "/Users/dev/Developer/shared-runtime");
    assert_eq!(entry.remote_path(), "/srv/_lab_workspaces/shared-runtime");
}

#[test]
fn preserves_dependency_role_and_identity_metadata() {
    let entry = workspace_mapping_entry_for_validation_dependency(&dependency_output());

    let value = serde_json::to_value(&entry).expect("serialize mapping entry");
    assert_eq!(value["role"], "validation_dependency");
    assert_eq!(value["snapshot_identity"], "shared-runtime");
    assert_eq!(
        value["dependency_freshness"]["source_provenance"],
        "validation_dependency_sibling"
    );
    assert_eq!(value["dependency_freshness"]["id"], "shared-runtime");
}
