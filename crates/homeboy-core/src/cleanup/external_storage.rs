//! Generic execution and policy for extension-owned external storage.
//!
//! The extension provider is the only code that understands its roots and its
//! durable store. Core validates inventory, applies lifecycle policy, and sends
//! selected identities back for native reclaim. It intentionally never calls
//! `remove_dir_all` on a provider locator.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use homeboy_engine_primitives::command::{
    wait_with_bounded_output_supervised, ControllerChildGuard, SupervisedCommandTermination,
};
use homeboy_extension_contract::{
    ExternalStorageInventory, ExternalStorageItem, ExternalStorageOperation,
    ExternalStorageReclaimResult, ExternalStorageReclaimTarget, ExternalStorageRequest,
    ExternalStorageResourceClass, ExternalStorageRetentionProviderConfig,
    EXTERNAL_STORAGE_RETENTION_SCHEMA,
};
use serde::Serialize;

use crate::{Error, Result};

const PROVIDER_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ExternalStorageCleanupOptions {
    pub apply: bool,
    pub min_age_days: u64,
    pub max_bytes: u64,
    pub reserve_bytes: u64,
    pub limit: usize,
    pub evidence_limit: usize,
    pub deadline: Option<SystemTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalStorageCleanupOutput {
    pub provider_count: usize,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub unknown_bytes: u64,
    pub providers: Vec<ExternalStorageProviderOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalStorageProviderOutput {
    pub provider_id: String,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub unknown_bytes: u64,
    pub candidates: Vec<ExternalStorageEvidence>,
    pub applied: Vec<ExternalStorageEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalStorageEvidence {
    pub id: String,
    pub class: ExternalStorageResourceClass,
    pub bytes: u64,
    pub locator: String,
}

/// Discover every installed provider and execute one bounded policy pass.
pub fn cleanup_external_storage_from_extensions(
    options: ExternalStorageCleanupOptions,
) -> Result<ExternalStorageCleanupOutput> {
    let providers = crate::extension_store::load_all_extensions()?
        .into_iter()
        .flat_map(|extension| extension.external_storage_retention.providers)
        .collect::<Vec<_>>();
    cleanup_external_storage_with_providers(&providers, options)
}

/// Execute explicit provider declarations. Kept public for deterministic
/// embeddings and tests; normal callers discover declarations from extensions.
pub fn cleanup_external_storage_with_providers(
    providers: &[ExternalStorageRetentionProviderConfig],
    options: ExternalStorageCleanupOptions,
) -> Result<ExternalStorageCleanupOutput> {
    let mut seen = HashSet::new();
    let mut output = ExternalStorageCleanupOutput {
        provider_count: providers.len(),
        candidate_count: 0,
        applied_count: 0,
        skipped_count: 0,
        estimated_bytes: 0,
        reclaimed_bytes: 0,
        unknown_bytes: 0,
        providers: Vec::new(),
    };
    for provider in providers {
        if !seen.insert(&provider.id) {
            return Err(Error::validation_invalid_argument(
                "external_storage_retention.providers",
                format!("duplicate external storage provider id '{}'", provider.id),
                None,
                None,
            ));
        }
        let inventory = invoke(
            provider,
            ExternalStorageOperation::Inventory,
            options.deadline,
        )?;
        if inventory.schema != EXTERNAL_STORAGE_RETENTION_SCHEMA
            || inventory.provider_id != provider.id
        {
            return Err(Error::validation_invalid_argument(
                "external_storage_retention provider response",
                "provider returned an unexpected schema or provider id",
                Some(provider.id.clone()),
                None,
            ));
        }
        let pressured_roots = pressured_roots(&inventory, options.reserve_bytes);
        let candidates = plan(
            &inventory.items,
            &pressured_roots,
            options.min_age_days,
            options.max_bytes,
            options.limit,
        );
        let estimated_bytes = candidates.iter().map(|item| item.bytes).sum();
        let targets = candidates
            .iter()
            .map(|item| ExternalStorageReclaimTarget {
                id: item.id.clone(),
                reclaim_token: item.reclaim_token.clone(),
            })
            .collect::<Vec<_>>();
        let (reclaimed_bytes, applied) = if options.apply && !candidates.is_empty() {
            // A provider performs deletion/compaction atomically in its own
            // model. Core's candidate bytes are accounting, not path authority.
            let reclaim =
                invoke_reclaim(provider, &inventory.generation, targets, options.deadline)?;
            if reclaim.schema != EXTERNAL_STORAGE_RETENTION_SCHEMA
                || reclaim.provider_id != provider.id
            {
                return Err(Error::internal_unexpected(
                    "external storage provider returned an invalid reclaim response",
                ));
            }
            let applied = validate_reclaim_receipt(&reclaim, &inventory.generation, &candidates)?;
            (reclaim.reclaimed_bytes, applied)
        } else {
            (0, Vec::new())
        };
        let candidate_evidence = candidates
            .iter()
            .take(options.evidence_limit)
            .map(|item| evidence(item))
            .collect();
        let provider_output = ExternalStorageProviderOutput {
            provider_id: provider.id.clone(),
            candidate_count: candidates.len(),
            applied_count: applied.len(),
            skipped_count: inventory.items.len().saturating_sub(candidates.len()),
            estimated_bytes,
            reclaimed_bytes,
            unknown_bytes: inventory.unknown_bytes,
            candidates: candidate_evidence,
            applied: applied
                .iter()
                .take(options.evidence_limit)
                .map(|item| evidence(item))
                .collect(),
        };
        output.candidate_count += provider_output.candidate_count;
        output.applied_count += provider_output.applied_count;
        output.skipped_count += provider_output.skipped_count;
        output.estimated_bytes += provider_output.estimated_bytes;
        output.reclaimed_bytes += provider_output.reclaimed_bytes;
        output.unknown_bytes += provider_output.unknown_bytes;
        output.providers.push(provider_output);
    }
    Ok(output)
}

fn plan<'a>(
    items: &'a [ExternalStorageItem],
    pressured_roots: &HashSet<String>,
    min_age_days: u64,
    max_bytes: u64,
    limit: usize,
) -> Vec<&'a ExternalStorageItem> {
    let mut selected = Vec::new();
    let mut selected_bytes = 0_u64;
    for item in items {
        if selected.len() >= limit || selected_bytes.saturating_add(item.bytes) > max_bytes {
            continue;
        }
        if !item.ownership_known
            || !item.reconstructable
            || item.active
            || item.referenced
            || matches!(
                item.class,
                ExternalStorageResourceClass::Credential
                    | ExternalStorageResourceClass::PinnedExport
            )
            || (!pressured_roots.contains(&item.root_id) && item.age_days < min_age_days)
        {
            continue;
        }
        selected_bytes = selected_bytes.saturating_add(item.bytes);
        selected.push(item);
    }
    selected
}

fn pressured_roots(inventory: &ExternalStorageInventory, reserve_bytes: u64) -> HashSet<String> {
    inventory
        .roots
        .iter()
        .filter_map(|root| {
            (reserve_bytes > 0
                && crate::observation::disk_budget::disk_budget(
                    std::path::Path::new(&root.path),
                    "external storage",
                    "provider root capacity is not measurable",
                )
                .available_bytes
                .is_some_and(|available| available < reserve_bytes))
            .then(|| root.id.clone())
        })
        .collect()
}

fn validate_reclaim_receipt<'a>(
    receipt: &ExternalStorageReclaimResult,
    generation: &str,
    candidates: &[&'a ExternalStorageItem],
) -> Result<Vec<&'a ExternalStorageItem>> {
    if receipt.generation != generation {
        return Err(Error::validation_invalid_argument(
            "external_storage_retention provider reclaim response",
            "provider rejected or did not echo the inventory generation",
            None,
            None,
        ));
    }
    let requested: HashMap<_, _> = candidates
        .iter()
        .map(|item| (item.id.as_str(), *item))
        .collect();
    let mut seen = HashSet::new();
    let mut applied = Vec::new();
    for id in &receipt.reclaimed_item_ids {
        if !seen.insert(id) {
            return Err(Error::validation_invalid_argument(
                "external_storage_retention provider reclaim response",
                "receipt contains duplicate item ids",
                None,
                None,
            ));
        }
        let Some(item) = requested.get(id.as_str()) else {
            return Err(Error::validation_invalid_argument(
                "external_storage_retention provider reclaim response",
                "receipt contains an item that was not requested",
                None,
                None,
            ));
        };
        applied.push(*item);
    }
    if receipt.reclaimed_bytes > applied.iter().map(|item| item.bytes).sum() {
        return Err(Error::validation_invalid_argument(
            "external_storage_retention provider reclaim response",
            "receipt bytes exceed confirmed item bytes",
            None,
            None,
        ));
    }
    Ok(applied)
}

fn evidence(item: &ExternalStorageItem) -> ExternalStorageEvidence {
    ExternalStorageEvidence {
        id: item.id.clone(),
        class: item.class.clone(),
        bytes: item.bytes,
        locator: item.locator.clone(),
    }
}

fn invoke(
    provider: &ExternalStorageRetentionProviderConfig,
    operation: ExternalStorageOperation,
    deadline: Option<SystemTime>,
) -> Result<ExternalStorageInventory> {
    let value = invoke_raw(provider, operation, None, Vec::new(), deadline)?;
    serde_json::from_slice(&value).map_err(|error| {
        Error::validation_invalid_argument(
            "external_storage_retention provider response",
            format!("invalid JSON: {error}"),
            Some(provider.id.clone()),
            None,
        )
    })
}

fn invoke_raw(
    provider: &ExternalStorageRetentionProviderConfig,
    operation: ExternalStorageOperation,
    generation: Option<String>,
    reclaim_targets: Vec<ExternalStorageReclaimTarget>,
    deadline: Option<SystemTime>,
) -> Result<Vec<u8>> {
    let Some((program, args)) = provider.command.split_first() else {
        return Err(Error::validation_invalid_argument(
            "external_storage_retention.providers.command",
            "provider command must not be empty",
            Some(provider.id.clone()),
            None,
        ));
    };
    let request = serde_json::to_vec(&ExternalStorageRequest {
        schema: EXTERNAL_STORAGE_RETENTION_SCHEMA.to_string(),
        operation,
        generation,
        reclaim_targets,
    })
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize external storage request".to_string()),
        )
    })?;
    let timeout = deadline
        .and_then(|deadline| deadline.duration_since(SystemTime::now()).ok())
        .map(|remaining| remaining.min(Duration::from_secs(provider.timeout_seconds)))
        .unwrap_or_else(|| Duration::from_secs(provider.timeout_seconds));
    if timeout.is_zero() {
        return Err(Error::internal_unexpected(
            "external storage retention deadline elapsed",
        ));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let guard = ControllerChildGuard::prepare(&mut command).map_err(|error| {
        Error::internal_unexpected(format!(
            "guard external storage provider '{}': {error}",
            provider.id
        ))
    })?;
    let mut child = command.spawn().map_err(|error| {
        Error::internal_unexpected(format!(
            "start external storage provider '{}': {error}",
            provider.id
        ))
    })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(&request).map_err(|error| {
        Error::internal_unexpected(format!(
            "write external storage provider '{}': {error}",
            provider.id
        ))
    })?;
    drop(stdin);
    guard.attach(&child).map_err(|error| {
        Error::internal_unexpected(format!(
            "attach external storage provider guard '{}': {error}",
            provider.id
        ))
    })?;
    let result = wait_with_bounded_output_supervised(
        &mut child,
        PROVIDER_OUTPUT_LIMIT,
        timeout,
        Duration::from_millis(100),
        || false,
        |_, _| Ok(()),
    )
    .map_err(|error| {
        Error::internal_unexpected(format!(
            "wait for external storage provider '{}': {error}",
            provider.id
        ))
    })?;
    if result.termination != SupervisedCommandTermination::Completed
        || !result.output.status.success()
    {
        return Err(Error::internal_unexpected(format!(
            "external storage provider '{}' failed or exceeded its output budget",
            provider.id
        )));
    }
    Ok(result.output.stdout)
}

fn invoke_reclaim(
    provider: &ExternalStorageRetentionProviderConfig,
    generation: &str,
    reclaim_targets: Vec<ExternalStorageReclaimTarget>,
    deadline: Option<SystemTime>,
) -> Result<ExternalStorageReclaimResult> {
    let value = invoke_raw(
        provider,
        ExternalStorageOperation::Reclaim,
        Some(generation.to_string()),
        reclaim_targets,
        deadline,
    )?;
    serde_json::from_slice(&value).map_err(|error| {
        Error::validation_invalid_argument(
            "external_storage_retention provider reclaim response",
            format!("invalid JSON: {error}"),
            Some(provider.id.clone()),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, class: ExternalStorageResourceClass) -> ExternalStorageItem {
        ExternalStorageItem {
            id: id.to_string(),
            root_id: "root".to_string(),
            class,
            bytes: 10,
            locator: id.to_string(),
            reconstructable: true,
            active: false,
            referenced: false,
            ownership_known: true,
            age_days: 7,
            reclaim_token: format!("token-{id}"),
        }
    }

    #[test]
    fn plan_reclaims_only_terminal_unreferenced_reconstructable_resources() {
        let mut active = item("active", ExternalStorageResourceClass::Scratch);
        active.active = true;
        let mut referenced = item("referenced", ExternalStorageResourceClass::DurableArtifact);
        referenced.referenced = true;
        let mut unknown = item("old-unmanaged", ExternalStorageResourceClass::Scratch);
        unknown.ownership_known = false;
        let inventory = vec![
            item("terminal-scratch", ExternalStorageResourceClass::Scratch),
            active,
            referenced,
            unknown,
            item("credentials", ExternalStorageResourceClass::Credential),
        ];
        let planned = plan(&inventory, &HashSet::new(), 0, 100, 10);
        assert_eq!(
            planned
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["terminal-scratch"]
        );
    }

    #[test]
    fn plan_is_bounded_before_evidence_is_rendered() {
        let inventory = vec![
            item("one", ExternalStorageResourceClass::Scratch),
            item("two", ExternalStorageResourceClass::Scratch),
        ];
        assert_eq!(plan(&inventory, &HashSet::new(), 0, 100, 1).len(), 1);
    }

    #[test]
    fn policy_applies_age_byte_ceiling_and_pressure_without_widening_liveness() {
        let old = item("old", ExternalStorageResourceClass::Scratch);
        let mut young = item("young", ExternalStorageResourceClass::Scratch);
        young.age_days = 0;
        let mut live = item("live", ExternalStorageResourceClass::Scratch);
        live.age_days = 0;
        live.active = true;
        let mut credential = item("credential", ExternalStorageResourceClass::Credential);
        credential.age_days = 99;
        let mut pinned = item("pinned", ExternalStorageResourceClass::PinnedExport);
        pinned.age_days = 99;
        let mut referenced = item("referenced", ExternalStorageResourceClass::DurableArtifact);
        referenced.referenced = true;
        let inventory = vec![old, young, live, credential, pinned, referenced];
        assert_eq!(
            plan(&inventory, &HashSet::new(), 7, 10, 10)
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["old"],
        );
        let pressured = HashSet::from(["root".to_string()]);
        assert_eq!(
            plan(&inventory, &pressured, 7, 20, 10)
                .iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["old", "young"],
            "reserve pressure bypasses age only; live, referenced, credential, and pinned resources remain protected",
        );
    }

    #[test]
    fn stale_generation_and_unconfirmed_receipts_are_rejected() {
        let candidate = item("scratch", ExternalStorageResourceClass::Scratch);
        let stale = ExternalStorageReclaimResult {
            schema: EXTERNAL_STORAGE_RETENTION_SCHEMA.to_string(),
            provider_id: "fixture".to_string(),
            generation: "old-generation".to_string(),
            reclaimed_item_ids: vec!["scratch".to_string()],
            reclaimed_bytes: 10,
        };
        assert!(validate_reclaim_receipt(&stale, "current-generation", &[&candidate]).is_err());
        let overclaim = ExternalStorageReclaimResult {
            generation: "current-generation".to_string(),
            reclaimed_item_ids: vec!["scratch".to_string(), "scratch".to_string()],
            ..stale
        };
        assert!(validate_reclaim_receipt(&overclaim, "current-generation", &[&candidate]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn provider_inventory_is_planned_and_reclaimed_without_path_deletion() {
        let receipt = tempfile::NamedTempFile::new().expect("receipt");
        let script = r#"input=$(cat); printf '%s' "$input" > "$1"; case "$input" in *'reclaim'*) printf '%s' '{"schema":"homeboy/external-storage-retention/v1","provider_id":"fixture","generation":"g1","reclaimed_item_ids":["scratch"],"reclaimed_bytes":12}' ;; *) printf '%s' '{"schema":"homeboy/external-storage-retention/v1","provider_id":"fixture","generation":"g1","unknown_bytes":99,"items":[{"id":"scratch","root_id":"tmp","class":"scratch","bytes":12,"locator":"external/tmp/scratch","reconstructable":true,"active":false,"referenced":false,"ownership_known":true,"age_days":7,"reclaim_token":"t1"},{"id":"live-db","root_id":"data","class":"session_store","bytes":58,"locator":"external/data/live.db","reconstructable":false,"active":true,"referenced":true,"ownership_known":true,"age_days":7,"reclaim_token":"t2"},{"id":"referenced-output","root_id":"data","class":"durable_artifact","bytes":11,"locator":"external/tool-output","reconstructable":true,"active":false,"referenced":true,"ownership_known":true,"age_days":7,"reclaim_token":"t3"},{"id":"credential","root_id":"data","class":"credential","bytes":2,"locator":"external/auth","reconstructable":true,"active":false,"referenced":false,"ownership_known":true,"age_days":7,"reclaim_token":"t4"},{"id":"pinned","root_id":"data","class":"pinned_export","bytes":3,"locator":"external/export","reconstructable":true,"active":false,"referenced":false,"ownership_known":true,"age_days":7,"reclaim_token":"t5"},{"id":"old-unmanaged","root_id":"tmp","class":"scratch","bytes":4,"locator":"external/old","reconstructable":true,"active":false,"referenced":false,"ownership_known":false,"age_days":7,"reclaim_token":"t6"}]}' ;; esac"#;
        let provider = ExternalStorageRetentionProviderConfig {
            id: "fixture".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                script.to_string(),
                "fixture".to_string(),
                receipt.path().display().to_string(),
            ],
            timeout_seconds: 1,
        };
        let output = cleanup_external_storage_with_providers(
            &[provider],
            ExternalStorageCleanupOptions {
                apply: true,
                min_age_days: 0,
                max_bytes: 100,
                reserve_bytes: 0,
                limit: 10,
                evidence_limit: 1,
                deadline: None,
            },
        )
        .expect("cleanup");
        assert_eq!(output.candidate_count, 1);
        assert_eq!(output.reclaimed_bytes, 12);
        assert_eq!(output.unknown_bytes, 99);
        assert_eq!(
            output.providers[0].candidates[0].locator,
            "external/tmp/scratch"
        );
        assert!(std::fs::read_to_string(receipt.path())
            .expect("receipt")
            .contains("reclaim"));
    }
}
