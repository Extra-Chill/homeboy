use crate::core::resource_cleanup_intent::ResourceCleanupIntent;
use crate::core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleIndex,
    ResourceLifecycleRecord, ResourceLifecycleResourceStatus, RESOURCE_LIFECYCLE_INDEX_SCHEMA,
};

use super::spec::RigResourcesSpec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigResourceLifecycleOptions {
    pub owner: String,
    pub run_id: String,
    pub runner_id: Option<String>,
    pub cleanup_intent: ResourceCleanupIntent,
    pub status: ResourceLifecycleResourceStatus,
}

impl RigResourceLifecycleOptions {
    pub fn new(run_id: impl Into<String>, status: ResourceLifecycleResourceStatus) -> Self {
        Self {
            owner: "homeboy.rig".to_string(),
            run_id: run_id.into(),
            runner_id: None,
            cleanup_intent: ResourceCleanupIntent::DryRun,
            status,
        }
    }
}

pub fn rig_resource_lifecycle_index(
    rig_id: &str,
    resources: &RigResourcesSpec,
    options: RigResourceLifecycleOptions,
) -> ResourceLifecycleIndex {
    ResourceLifecycleIndex {
        schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
        resources: rig_resource_lifecycle_records(rig_id, resources, options),
    }
}

pub fn rig_resource_lifecycle_records(
    rig_id: &str,
    resources: &RigResourcesSpec,
    options: RigResourceLifecycleOptions,
) -> Vec<ResourceLifecycleRecord> {
    let mut records = Vec::new();

    for token in &resources.exclusive {
        records.push(record(
            &options,
            "rig_exclusive",
            format!("rig://{rig_id}/exclusive/{token}"),
        ));
    }
    for path in &resources.paths {
        records.push(record(&options, "rig_path", path.clone()));
    }
    for port in &resources.ports {
        records.push(record(
            &options,
            "rig_port",
            format!("tcp://localhost:{port}"),
        ));
    }
    for pattern in &resources.process_patterns {
        records.push(record(
            &options,
            "rig_process_pattern",
            format!("process-pattern:{pattern}"),
        ));
    }

    records
}

fn record(
    options: &RigResourceLifecycleOptions,
    kind: &str,
    path: String,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: options.owner.clone(),
        run_id: options.run_id.clone(),
        runner_id: options.runner_id.clone(),
        path,
        root_bound: None,
        kind: kind.to_string(),
        ttl: None,
        cleanup_policy: ResourceCleanupPolicy::Manual,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: options.cleanup_intent,
        cleanup_command: Some(format!(
            "homeboy runs resources --run-id {} --cleanup-plan",
            options.run_id
        )),
        status: options.status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rig_resource_declarations_to_lifecycle_records() {
        let resources = RigResourcesSpec {
            exclusive: vec!["runtime".to_string()],
            paths: vec!["/tmp/homeboy-rig".to_string()],
            ports: vec![9981],
            process_patterns: vec!["homeboy fixture".to_string()],
        };

        let index = rig_resource_lifecycle_index(
            "fixture-rig",
            &resources,
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
        );

        index.validate().expect("contract-valid lifecycle index");
        assert_eq!(index.resources.len(), 4);
        assert_eq!(index.resources[0].owner, "homeboy.rig");
        assert_eq!(
            index.resources[0].path,
            "rig://fixture-rig/exclusive/runtime"
        );
        assert_eq!(index.resources[1].kind, "rig_path");
        assert_eq!(index.resources[1].path, "/tmp/homeboy-rig");
        assert_eq!(index.resources[2].path, "tcp://localhost:9981");
        assert_eq!(index.resources[3].path, "process-pattern:homeboy fixture");
        assert_eq!(
            index.resources[0].cleanup_policy,
            ResourceCleanupPolicy::Manual
        );
    }

    #[test]
    fn applies_declared_cleanup_intent_to_resource_records() {
        let resources = RigResourcesSpec {
            paths: vec!["/tmp/homeboy-rig".to_string()],
            ..Default::default()
        };
        let mut options =
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active);
        options.cleanup_intent = ResourceCleanupIntent::Apply;

        let index = rig_resource_lifecycle_index("fixture-rig", &resources, options);

        assert_eq!(
            index.resources[0].cleanup_intent,
            ResourceCleanupIntent::Apply
        );
    }
}
