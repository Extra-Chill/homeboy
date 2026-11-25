use homeboy_core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy_core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleIndex,
    ResourceLifecycleRecord, ResourceLifecycleResourceStatus, RESOURCE_LIFECYCLE_INDEX_SCHEMA,
};

use super::spec::{
    RigResourceRetentionSpec, RigResourcesSpec, RIG_RESOURCE_CLASS_EXCLUSIVE,
    RIG_RESOURCE_CLASS_LIFECYCLE_SNAPSHOTS, RIG_RESOURCE_CLASS_PATHS, RIG_RESOURCE_CLASS_PORTS,
    RIG_RESOURCE_CLASS_PROCESS_PATTERNS,
};
use super::state::LifecycleSnapshotState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RigResourceLifecycleOptions {
    pub owner: String,
    pub run_id: String,
    pub runner_id: Option<String>,
    pub cleanup_intent: ResourceCleanupIntent,
    pub status: ResourceLifecycleResourceStatus,
}

impl RigResourceLifecycleOptions {
    pub(crate) fn new(run_id: impl Into<String>, status: ResourceLifecycleResourceStatus) -> Self {
        Self {
            owner: "homeboy.rig".to_string(),
            run_id: run_id.into(),
            runner_id: None,
            cleanup_intent: ResourceCleanupIntent::DryRun,
            status,
        }
    }
}

pub(crate) fn rig_resource_lifecycle_index(
    rig_id: &str,
    resources: &RigResourcesSpec,
    options: RigResourceLifecycleOptions,
) -> ResourceLifecycleIndex {
    ResourceLifecycleIndex {
        schema: RESOURCE_LIFECYCLE_INDEX_SCHEMA.to_string(),
        resources: rig_resource_lifecycle_records(rig_id, resources, options),
    }
}

pub(crate) fn rig_resource_lifecycle_records(
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
pub(crate) fn dependency_materialization_cache_lifecycle_record(
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

/// Default retention for a live lifecycle snapshot handle.
///
/// A snapshot handle names a disposable environment. Nothing reclaims it if the
/// run that created it dies before its `teardown` phase, so unlike the declared
/// resource classes (which default to `manual`) the safe default here is a TTL
/// reap. A rig tightens or loosens it through
/// `resources.lifecycle_by_class.lifecycle_snapshots`.
const LIFECYCLE_SNAPSHOT_DEFAULT_TTL: &str = "P1D";

/// Resource kind recorded for a live lifecycle snapshot handle.
pub(crate) const LIFECYCLE_SNAPSHOT_RESOURCE_KIND: &str = "lifecycle_snapshot";

/// Lifecycle records for the snapshot handles a rig currently holds.
///
/// These are the leaked-sandbox reaping records: each live handle becomes a
/// resource `homeboy runs resources --cleanup-plan` can see and act on. The
/// handle's own `locator` is the resource path when the runtime supplied one,
/// otherwise a rig-scoped URI, so the record is always addressable without
/// Homeboy inventing knowledge about the environment.
pub(crate) fn lifecycle_snapshot_lifecycle_records<'a>(
    rig_id: &str,
    resources: &RigResourcesSpec,
    options: &RigResourceLifecycleOptions,
    snapshots: impl IntoIterator<Item = (&'a String, &'a LifecycleSnapshotState)>,
) -> Vec<ResourceLifecycleRecord> {
    let retention = snapshot_retention(resources);

    snapshots
        .into_iter()
        .map(|(id, entry)| {
            let (cleanup_policy, ttl) = retention.clone();
            ResourceLifecycleRecord {
                owner: "homeboy.rig.lifecycle_snapshot".to_string(),
                run_id: options.run_id.clone(),
                runner_id: options.runner_id.clone(),
                path: entry
                    .snapshot
                    .locator
                    .as_deref()
                    .map(str::trim)
                    .filter(|locator| !locator.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("rig://{rig_id}/lifecycle-snapshot/{id}")),
                root_bound: None,
                kind: LIFECYCLE_SNAPSHOT_RESOURCE_KIND.to_string(),
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
        })
        .collect()
}

/// Retention for lifecycle snapshot handles: declared override, then the rig
/// default, then the TTL-reap fallback.
fn snapshot_retention(resources: &RigResourcesSpec) -> (ResourceCleanupPolicy, Option<String>) {
    let declared = resources.retention_for_class(RIG_RESOURCE_CLASS_LIFECYCLE_SNAPSHOTS);
    let ttl = declared
        .ttl
        .as_ref()
        .map(|ttl| ttl.trim().to_string())
        .filter(|ttl| !ttl.is_empty());
    let policy = declared
        .cleanup_policy
        .unwrap_or(ResourceCleanupPolicy::DeleteAfterTtl);

    // Same hardening as the declared resource classes: `delete_after_ttl`
    // without a ttl is contract-invalid and would sink the whole index, so
    // supply the default ttl rather than emitting an unvalidatable record.
    if matches!(policy, ResourceCleanupPolicy::DeleteAfterTtl) && ttl.is_none() {
        return (
            ResourceCleanupPolicy::DeleteAfterTtl,
            Some(LIFECYCLE_SNAPSHOT_DEFAULT_TTL.to_string()),
        );
    }

    (policy, ttl)
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
        let mut options =
            RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active);
        options.runner_id = Some("runner-abc".to_string());

        let index = rig_resource_lifecycle_index("fixture-rig", &resources, options);

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
        // Specs without retention declarations keep manual, ttl-less records.
        for record in &index.resources {
            assert_eq!(record.runner_id.as_deref(), Some("runner-abc"));
            assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::Manual);
            assert_eq!(record.ttl, None, "{} must keep no ttl", record.kind);
        }
    }

    #[test]
    fn applies_declared_cleanup_intent_to_resource_records() {
        for (cleanup, expected) in [
            (
                serde_json::json!({"intent": "apply"}),
                ResourceCleanupIntent::Apply,
            ),
            (
                serde_json::json!({"intent": "dry_run"}),
                ResourceCleanupIntent::DryRun,
            ),
            (
                serde_json::json!({"intent": "pipeline"}),
                ResourceCleanupIntent::DryRun,
            ),
            (
                serde_json::json!({"intent": "external"}),
                ResourceCleanupIntent::DryRun,
            ),
            (
                serde_json::json!({"reason": "manual by default"}),
                ResourceCleanupIntent::DryRun,
            ),
        ] {
            let lifecycle: super::super::spec::RigLifecycleSpec =
                serde_json::from_value(serde_json::json!({"cleanup": cleanup}))
                    .expect("canonical cleanup declaration");
            let mut options =
                RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active);
            options.cleanup_intent = lifecycle
                .cleanup
                .expect("declared cleanup")
                .resource_cleanup_intent();
            let index = rig_resource_lifecycle_index("fixture-rig", &fixture_resources(), options);
            index.validate().expect("contract-valid lifecycle index");
            assert_eq!(index.resources.len(), 4);
            for record in &index.resources {
                assert_eq!(
                    record.cleanup_intent, expected,
                    "cleanup intent for {}",
                    record.kind
                );
            }
        }
    }

    #[test]
    fn declared_retention_resolves_per_class_over_defaults() {
        let delete_after_ttl = Some(ResourceCleanupPolicy::DeleteAfterTtl);
        // (default retention, ports override, expected port, expected other classes)
        let cases = [
            (
                RigResourceRetentionSpec {
                    ttl: Some("PT1H".to_string()),
                    cleanup_policy: delete_after_ttl,
                },
                None,
                (ResourceCleanupPolicy::DeleteAfterTtl, Some("PT1H")),
                (ResourceCleanupPolicy::DeleteAfterTtl, Some("PT1H")),
            ),
            (
                RigResourceRetentionSpec::default(),
                Some(RigResourceRetentionSpec {
                    ttl: Some("PT1H".to_string()),
                    cleanup_policy: delete_after_ttl,
                }),
                (ResourceCleanupPolicy::DeleteAfterTtl, Some("PT1H")),
                (ResourceCleanupPolicy::Manual, None),
            ),
            (
                RigResourceRetentionSpec {
                    ttl: Some("P1D".to_string()),
                    cleanup_policy: None,
                },
                Some(RigResourceRetentionSpec {
                    ttl: None,
                    cleanup_policy: delete_after_ttl,
                }),
                (ResourceCleanupPolicy::DeleteAfterTtl, Some("P1D")),
                (ResourceCleanupPolicy::Manual, Some("P1D")),
            ),
        ];

        for (default, ports, expected_port, expected_other) in cases {
            let mut resources = RigResourcesSpec {
                lifecycle: default,
                ..fixture_resources()
            };
            if let Some(ports) = ports {
                resources
                    .lifecycle_by_class
                    .insert(RIG_RESOURCE_CLASS_PORTS.to_string(), ports);
            }

            let index = rig_resource_lifecycle_index(
                "fixture-rig",
                &resources,
                RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
            );

            index.validate().expect("contract-valid lifecycle index");
            for record in &index.resources {
                let (policy, ttl) = if record.kind == "rig_port" {
                    expected_port
                } else {
                    expected_other
                };
                assert_eq!(record.cleanup_policy, policy, "{}", record.kind);
                assert_eq!(record.ttl.as_deref(), ttl, "{}", record.kind);
            }
        }
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

    fn snapshot_state(id: &str, locator: Option<&str>) -> LifecycleSnapshotState {
        LifecycleSnapshotState {
            step: "provision".to_string(),
            component: None,
            captured_at: "2026-07-27T13:00:00Z".to_string(),
            snapshot: homeboy_lifecycle_contract::LifecycleSnapshotRef {
                schema: "homeboy/lifecycle-snapshot-ref/v1".to_string(),
                id: id.to_string(),
                kind: "lifecycle_snapshot".to_string(),
                phase_id: Some("capture".to_string()),
                artifact_id: None,
                artifact: None,
                locator: locator.map(str::to_string),
                created_at: Some("2026-07-27T13:00:00Z".to_string()),
                metadata: Default::default(),
            },
        }
    }

    fn snapshot_records(
        resources: &RigResourcesSpec,
        snapshots: &[(&str, Option<&str>)],
    ) -> Vec<ResourceLifecycleRecord> {
        let snapshots: std::collections::BTreeMap<String, LifecycleSnapshotState> = snapshots
            .iter()
            .map(|(id, locator)| (id.to_string(), snapshot_state(id, *locator)))
            .collect();
        lifecycle_snapshot_lifecycle_records(
            "fixture-rig",
            resources,
            &RigResourceLifecycleOptions::new("run-1", ResourceLifecycleResourceStatus::Active),
            snapshots.iter(),
        )
    }

    #[test]
    fn live_snapshot_handles_become_ttl_reapable_resources() {
        let records = snapshot_records(
            &RigResourcesSpec::default(),
            &[
                ("capture", None),
                ("sandbox-7f3", Some("opaque://sandbox/7f3")),
            ],
        );

        assert_eq!(records.len(), 2);
        for record in &records {
            record.validate(0).expect("valid snapshot record");
            assert_eq!(record.kind, "lifecycle_snapshot");
            assert_eq!(record.owner, "homeboy.rig.lifecycle_snapshot");
            assert_eq!(record.cleanup_policy, ResourceCleanupPolicy::DeleteAfterTtl);
            assert_eq!(record.ttl.as_deref(), Some("P1D"));
        }
        // Handles without a locator get an addressable rig URI.
        assert_eq!(
            records[0].path,
            "rig://fixture-rig/lifecycle-snapshot/capture"
        );
        assert_eq!(records[1].path, "opaque://sandbox/7f3");
    }

    /// The TTL-reap posture is a default, not a policy homeboy imposes.
    #[test]
    fn declared_snapshot_retention_overrides_the_ttl_reap_default() {
        for (declared, expected_policy, expected_ttl) in [
            (
                RigResourceRetentionSpec {
                    ttl: Some("PT30M".to_string()),
                    cleanup_policy: Some(ResourceCleanupPolicy::DeleteAfterTtl),
                },
                ResourceCleanupPolicy::DeleteAfterTtl,
                Some("PT30M"),
            ),
            (
                RigResourceRetentionSpec {
                    ttl: None,
                    cleanup_policy: Some(ResourceCleanupPolicy::Preserve),
                },
                ResourceCleanupPolicy::Preserve,
                None,
            ),
        ] {
            let mut resources = RigResourcesSpec::default();
            resources.lifecycle_by_class.insert(
                crate::spec::RIG_RESOURCE_CLASS_LIFECYCLE_SNAPSHOTS.to_string(),
                declared,
            );

            let records = snapshot_records(&resources, &[("capture", None)]);

            records[0].validate(0).expect("valid snapshot record");
            assert_eq!(records[0].cleanup_policy, expected_policy);
            assert_eq!(records[0].ttl.as_deref(), expected_ttl);
        }
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
