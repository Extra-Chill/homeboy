use homeboy_core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy_core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleIndex,
    ResourceLifecycleRecord, ResourceLifecycleResourceStatus, RESOURCE_LIFECYCLE_INDEX_SCHEMA,
};

use super::spec::{
    RigResourceRetentionSpec, RigResourcesSpec, RIG_RESOURCE_CLASS_EXCLUSIVE,
    RIG_RESOURCE_CLASS_PATHS, RIG_RESOURCE_CLASS_PORTS, RIG_RESOURCE_CLASS_PROCESS_PATTERNS,
};

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

    let exclusive_retention = resources.retention_for_class(RIG_RESOURCE_CLASS_EXCLUSIVE);
    let path_retention = resources.retention_for_class(RIG_RESOURCE_CLASS_PATHS);
    let port_retention = resources.retention_for_class(RIG_RESOURCE_CLASS_PORTS);
    let process_pattern_retention =
        resources.retention_for_class(RIG_RESOURCE_CLASS_PROCESS_PATTERNS);

    for token in &resources.exclusive {
        records.push(record(
            &options,
            "rig_exclusive",
            format!("rig://{rig_id}/exclusive/{token}"),
            &exclusive_retention,
        ));
    }
    for path in &resources.paths {
        records.push(record(&options, "rig_path", path.clone(), &path_retention));
    }
    for port in &resources.ports {
        records.push(record(
            &options,
            "rig_port",
            format!("tcp://localhost:{port}"),
            &port_retention,
        ));
    }
    for pattern in &resources.process_patterns {
        records.push(record(
            &options,
            "rig_process_pattern",
            format!("process-pattern:{pattern}"),
            &process_pattern_retention,
        ));
    }

    records
}

/// Add durable dependency-cache storage to the same lifecycle index as rig
/// resources. Entries are content addressed and independent of a run workspace,
/// so retention is TTL based rather than terminal-run cleanup.
pub fn dependency_materialization_cache_lifecycle_record(
    options: &RigResourceLifecycleOptions,
    root: &std::path::Path,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: "homeboy.rig.dependency_materialization_cache".to_string(),
        run_id: options.run_id.clone(),
        runner_id: options.runner_id.clone(),
        path: root.display().to_string(),
        // The cache root itself is the removable owned resource; bind cleanup
        // to its parent so lifecycle cleanup cannot escape Homeboy cache space.
        root_bound: root.parent().map(|parent| parent.display().to_string()),
        kind: "dependency_materialization_cache".to_string(),
        ttl: Some("P30D".to_string()),
        cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
        evidence_retention: ResourceEvidenceRetention::Manifest,
        // Cache ownership is explicit: dry-run commands still only plan, while
        // an operator's --apply may reclaim expired content-addressed entries.
        cleanup_intent: ResourceCleanupIntent::Apply,
        cleanup_command: Some(format!(
            "homeboy runs resources --run-id {} --cleanup-plan",
            options.run_id
        )),
        status: options.status,
    }
}

fn record(
    options: &RigResourceLifecycleOptions,
    kind: &str,
    path: String,
    retention: &RigResourceRetentionSpec,
) -> ResourceLifecycleRecord {
    let (cleanup_policy, ttl) = resolved_retention(retention);
    ResourceLifecycleRecord {
        owner: options.owner.clone(),
        run_id: options.run_id.clone(),
        runner_id: options.runner_id.clone(),
        path,
        root_bound: None,
        kind: kind.to_string(),
        ttl,
        cleanup_policy,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: options.cleanup_intent,
        cleanup_command: Some(format!(
            "homeboy runs resources --run-id {} --cleanup-plan",
            options.run_id
        )),
        status: options.status,
    }
}

/// Resolve a declared retention into the concrete lifecycle record fields.
///
/// Absent declarations keep the historical posture (`manual`, no ttl). A
/// `delete_after_ttl` policy without a ttl is contract-invalid, and emitting it
/// would make the whole lifecycle index fail validation and disappear, so it
/// degrades to `manual` here. `homeboy rig lint` reports the misdeclaration.
fn resolved_retention(
    retention: &RigResourceRetentionSpec,
) -> (ResourceCleanupPolicy, Option<String>) {
    let ttl = retention
        .ttl
        .as_ref()
        .map(|ttl| ttl.trim().to_string())
        .filter(|ttl| !ttl.is_empty());
    let policy = retention
        .cleanup_policy
        .unwrap_or(ResourceCleanupPolicy::Manual);
    if matches!(policy, ResourceCleanupPolicy::DeleteAfterTtl) && ttl.is_none() {
        return (ResourceCleanupPolicy::Manual, None);
    }
    (policy, ttl)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_resources() -> RigResourcesSpec {
        RigResourcesSpec {
            exclusive: vec!["runtime".to_string()],
            paths: vec!["/tmp/homeboy-rig".to_string()],
            ports: vec![9981],
            process_patterns: vec!["homeboy fixture".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn converts_rig_resource_declarations_to_lifecycle_records() {
        let resources = fixture_resources();

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
        assert_eq!(index.resources[0].runner_id, None);
        assert_eq!(
            index.resources[0].cleanup_policy,
            ResourceCleanupPolicy::Manual
        );
    }

    #[test]
    fn propagates_runner_id_to_resource_records() {
        let resources = fixture_resources();

        let mut options =
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active);
        options.runner_id = Some("runner-abc".to_string());

        let index = rig_resource_lifecycle_index("fixture-rig", &resources, options);

        for record in &index.resources {
            assert_eq!(record.runner_id.as_deref(), Some("runner-abc"));
        }
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

    /// Non-breaking guarantee: a spec that declares no `ttl`/`cleanup_policy`
    /// produces byte-identical lifecycle records to the pre-retention behavior
    /// (`manual`, no ttl) for every resource class.
    #[test]
    fn specs_without_retention_declarations_keep_manual_records() {
        let resources = fixture_resources();
        assert!(resources.lifecycle.is_empty());
        assert!(resources.lifecycle_by_class.is_empty());

        let records = rig_resource_lifecycle_records(
            "fixture-rig",
            &resources,
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
        );

        assert_eq!(records.len(), 4);
        for record in &records {
            assert_eq!(
                record.cleanup_policy,
                ResourceCleanupPolicy::Manual,
                "{} must keep the historical manual policy",
                record.kind
            );
            assert_eq!(record.ttl, None, "{} must keep no ttl", record.kind);
        }
    }

    #[test]
    fn applies_declared_retention_defaults_to_every_class() {
        let resources = RigResourcesSpec {
            lifecycle: RigResourceRetentionSpec {
                ttl: Some("PT1H".to_string()),
                cleanup_policy: Some(ResourceCleanupPolicy::DeleteAfterTtl),
            },
            ..fixture_resources()
        };

        let index = rig_resource_lifecycle_index(
            "fixture-rig",
            &resources,
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
        );

        index.validate().expect("contract-valid lifecycle index");
        for record in &index.resources {
            assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::DeleteAfterTtl);
            assert_eq!(record.ttl.as_deref(), Some("PT1H"));
        }
    }

    #[test]
    fn per_class_retention_overrides_only_its_own_class() {
        let mut resources = fixture_resources();
        resources.lifecycle_by_class.insert(
            RIG_RESOURCE_CLASS_PORTS.to_string(),
            RigResourceRetentionSpec {
                ttl: Some("PT1H".to_string()),
                cleanup_policy: Some(ResourceCleanupPolicy::DeleteAfterTtl),
            },
        );

        let index = rig_resource_lifecycle_index(
            "fixture-rig",
            &resources,
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
        );

        index.validate().expect("contract-valid lifecycle index");
        for record in &index.resources {
            if record.kind == "rig_port" {
                assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::DeleteAfterTtl);
                assert_eq!(record.ttl.as_deref(), Some("PT1H"));
            } else {
                assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::Manual);
                assert_eq!(record.ttl, None);
            }
        }
    }

    #[test]
    fn per_class_retention_inherits_unset_fields_from_the_default() {
        let mut resources = fixture_resources();
        resources.lifecycle.ttl = Some("P1D".to_string());
        resources.lifecycle_by_class.insert(
            RIG_RESOURCE_CLASS_PORTS.to_string(),
            RigResourceRetentionSpec {
                ttl: None,
                cleanup_policy: Some(ResourceCleanupPolicy::DeleteAfterTtl),
            },
        );

        let index = rig_resource_lifecycle_index(
            "fixture-rig",
            &resources,
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
        );

        index.validate().expect("contract-valid lifecycle index");
        let port = index
            .resources
            .iter()
            .find(|record| record.kind == "rig_port")
            .expect("port record");
        assert_eq!(port.cleanup_policy, ResourceCleanupPolicy::DeleteAfterTtl);
        assert_eq!(port.ttl.as_deref(), Some("P1D"));
    }

    /// `delete_after_ttl` without a ttl is contract-invalid. Degrading to
    /// `manual` keeps the whole index emittable instead of silently dropping
    /// every lifecycle record for the run.
    #[test]
    fn delete_after_ttl_without_ttl_degrades_to_manual() {
        let resources = RigResourcesSpec {
            lifecycle: RigResourceRetentionSpec {
                ttl: None,
                cleanup_policy: Some(ResourceCleanupPolicy::DeleteAfterTtl),
            },
            ..fixture_resources()
        };

        let index = rig_resource_lifecycle_index(
            "fixture-rig",
            &resources,
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
        );

        index.validate().expect("contract-valid lifecycle index");
        for record in &index.resources {
            assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::Manual);
            assert_eq!(record.ttl, None);
        }
    }

    #[test]
    fn retention_declarations_survive_spec_round_trip() {
        let mut resources = RigResourcesSpec {
            lifecycle: RigResourceRetentionSpec {
                ttl: None,
                cleanup_policy: Some(ResourceCleanupPolicy::DeleteOnTerminal),
            },
            ..Default::default()
        };
        resources.lifecycle_by_class.insert(
            RIG_RESOURCE_CLASS_PORTS.to_string(),
            RigResourceRetentionSpec {
                ttl: Some("PT1H".to_string()),
                cleanup_policy: Some(ResourceCleanupPolicy::DeleteAfterTtl),
            },
        );

        // No resources declared, but the block still carries information.
        assert!(resources.is_empty());
        assert!(!resources.is_unset());

        let json = serde_json::to_string(&resources).expect("serialize");
        let parsed: RigResourcesSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, resources);
    }

    #[test]
    fn dependency_cache_is_retained_through_the_lifecycle_contract() {
        let root = tempfile::tempdir().expect("cache root");
        let record = dependency_materialization_cache_lifecycle_record(
            &RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
            root.path(),
        );

        record.validate(0).expect("valid cache lifecycle record");
        assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::DeleteAfterTtl);
        assert_eq!(record.ttl.as_deref(), Some("P30D"));
        assert_eq!(
            record.evidence_retention,
            ResourceEvidenceRetention::Manifest
        );
    }
}
