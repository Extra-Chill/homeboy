//! Generic execution and policy for extension-owned external storage.
//!
//! The extension provider is the only code that understands its roots and its
//! durable store. Core validates inventory, applies lifecycle policy, and sends
//! selected identities back for native reclaim. It intentionally never calls
//! `remove_dir_all` on a provider locator.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use homeboy_engine_primitives::command::{
    terminate_process_tree_and_reap, wait_with_bounded_output_supervised, ControllerChildGuard,
    SupervisedCommandTermination,
};
use homeboy_extension_contract::{
    ExternalStorageInventory, ExternalStorageItem, ExternalStorageOperation,
    ExternalStorageReclaimResult, ExternalStorageReclaimTarget, ExternalStorageRequest,
    ExternalStorageResourceClass, ExternalStorageRetentionProviderConfig,
    EXTERNAL_STORAGE_RETENTION_SCHEMA, MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS,
    MAX_EXTERNAL_STORAGE_REQUEST_BYTES,
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
        provider_count: providers.len().min(options.limit),
        candidate_count: 0,
        applied_count: 0,
        skipped_count: 0,
        estimated_bytes: 0,
        reclaimed_bytes: 0,
        unknown_bytes: 0,
        providers: Vec::new(),
    };
    let mut remaining_count = options.limit;
    let mut remaining_bytes = options.max_bytes;
    let mut remaining_evidence = options.evidence_limit;
    for provider in providers.iter().take(options.limit) {
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
        validate_inventory_item_ids(&inventory)?;
        let pressured_roots = pressured_roots(&inventory, options.reserve_bytes);
        let candidates = plan(
            &inventory.items,
            &pressured_roots,
            options.min_age_days,
            remaining_bytes,
            remaining_count,
            &inventory.generation,
        );
        let estimated_bytes = checked_sum(
            candidates.iter().map(|item| item.bytes),
            "planned external storage bytes",
        )?;
        remaining_count = remaining_count.saturating_sub(candidates.len());
        remaining_bytes = remaining_bytes
            .checked_sub(estimated_bytes)
            .ok_or_else(|| {
                Error::internal_unexpected(
                    "external storage plan exceeded its aggregate byte ceiling",
                )
            })?;
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
            .take(remaining_evidence)
            .map(|item| evidence(item))
            .collect::<Vec<_>>();
        remaining_evidence = remaining_evidence.saturating_sub(candidate_evidence.len());
        let applied_evidence = applied
            .iter()
            .take(remaining_evidence)
            .map(|item| evidence(item))
            .collect::<Vec<_>>();
        remaining_evidence = remaining_evidence.saturating_sub(applied_evidence.len());
        let provider_output = ExternalStorageProviderOutput {
            provider_id: provider.id.clone(),
            candidate_count: candidates.len(),
            applied_count: applied.len(),
            skipped_count: inventory.items.len().saturating_sub(candidates.len()),
            estimated_bytes,
            reclaimed_bytes,
            unknown_bytes: inventory.unknown_bytes,
            candidates: candidate_evidence,
            applied: applied_evidence,
        };
        output.candidate_count += provider_output.candidate_count;
        output.applied_count += provider_output.applied_count;
        output.skipped_count += provider_output.skipped_count;
        output.estimated_bytes = checked_add(
            output.estimated_bytes,
            provider_output.estimated_bytes,
            "external storage estimated bytes",
        )?;
        output.reclaimed_bytes = checked_add(
            output.reclaimed_bytes,
            provider_output.reclaimed_bytes,
            "external storage reclaimed bytes",
        )?;
        output.unknown_bytes = checked_add(
            output.unknown_bytes,
            provider_output.unknown_bytes,
            "external storage unknown bytes",
        )?;
        output.providers.push(provider_output);
    }
    Ok(output)
}

fn validate_inventory_item_ids(inventory: &ExternalStorageInventory) -> Result<()> {
    let mut seen = HashSet::new();
    let mut duplicates = BTreeSet::new();
    for item in &inventory.items {
        if !seen.insert(item.id.as_str()) {
            duplicates.insert(item.id.as_str());
        }
    }
    if let Some(id) = duplicates.into_iter().next() {
        return Err(Error::validation_invalid_argument(
            "external_storage_retention provider inventory",
            format!(
                "provider '{}' returned duplicate item id '{id}'",
                inventory.provider_id
            ),
            Some(inventory.provider_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn plan<'a>(
    items: &'a [ExternalStorageItem],
    pressured_roots: &HashSet<String>,
    min_age_days: u64,
    max_bytes: u64,
    limit: usize,
    generation: &str,
) -> Vec<&'a ExternalStorageItem> {
    let mut selected = Vec::new();
    let mut selected_bytes = 0_u64;
    for item in items {
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
        if selected.len() >= limit.min(MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS) {
            break;
        }
        let Some(next_bytes) = selected_bytes.checked_add(item.bytes) else {
            continue;
        };
        if next_bytes > max_bytes {
            continue;
        }
        if !reclaim_request_fits(
            generation,
            selected
                .iter()
                .copied()
                .chain(std::iter::once(item))
                .map(|item| ExternalStorageReclaimTarget {
                    id: item.id.clone(),
                    reclaim_token: item.reclaim_token.clone(),
                }),
        ) {
            continue;
        }
        selected_bytes = next_bytes;
        selected.push(item);
    }
    selected
}

fn checked_add(left: u64, right: u64, subject: &str) -> Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| Error::internal_unexpected(format!("{subject} overflow")))
}

fn checked_sum(mut values: impl Iterator<Item = u64>, subject: &str) -> Result<u64> {
    values.try_fold(0_u64, |total, value| checked_add(total, value, subject))
}

fn reclaim_request_fits(
    generation: &str,
    reclaim_targets: impl Iterator<Item = ExternalStorageReclaimTarget>,
) -> bool {
    serde_json::to_vec(&ExternalStorageRequest {
        schema: EXTERNAL_STORAGE_RETENTION_SCHEMA.to_string(),
        operation: ExternalStorageOperation::Reclaim,
        generation: Some(generation.to_string()),
        reclaim_targets: reclaim_targets.collect(),
    })
    .is_ok_and(|request| request.len() <= MAX_EXTERNAL_STORAGE_REQUEST_BYTES)
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
    if receipt.reclaimed_bytes
        > checked_sum(
            applied.iter().map(|item| item.bytes),
            "confirmed external storage bytes",
        )?
    {
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
    if reclaim_targets.len() > MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS {
        return Err(Error::validation_invalid_argument(
            "external_storage_retention provider request",
            "reclaim request exceeds the protocol target ceiling",
            Some(provider.id.clone()),
            None,
        ));
    }
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
    if request.len() > MAX_EXTERNAL_STORAGE_REQUEST_BYTES {
        return Err(Error::validation_invalid_argument(
            "external_storage_retention provider request",
            "reclaim request exceeds the protocol target or byte ceiling",
            Some(provider.id.clone()),
            None,
        ));
    }
    let remaining = match deadline {
        Some(deadline) => deadline.duration_since(SystemTime::now()).map_err(|_| {
            Error::internal_unexpected("external storage retention deadline elapsed")
        })?,
        None => Duration::from_secs(provider.timeout_seconds),
    };
    let timeout = remaining.min(Duration::from_secs(provider.timeout_seconds));
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
    attach_guard_or_reap(
        &mut child,
        |child| guard.attach(child),
        terminate_process_tree_and_reap,
    )
    .map_err(|error| {
        Error::internal_unexpected(format!(
            "attach external storage provider guard '{}': {error}",
            provider.id
        ))
    })?;
    // Start stdin delivery only after process-tree ownership is established.
    // The writer can block on a provider that never reads, while the supervisor
    // concurrently drains output and kills the tree at the shared deadline.
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || stdin.write_all(&request));
    let supervised = wait_with_bounded_output_supervised(
        &mut child,
        PROVIDER_OUTPUT_LIMIT,
        timeout,
        Duration::from_millis(100),
        || false,
        |_, _| Ok(()),
    );
    // Always join delivery, including a supervision I/O failure, so no writer
    // remains detached after the process tree has been reaped.
    let write_result = writer.join().map_err(|_| {
        Error::internal_unexpected("external storage provider stdin writer panicked")
    })?;
    let result = supervised.map_err(|error| {
        Error::internal_unexpected(format!(
            "wait for external storage provider '{}': {error}",
            provider.id
        ))
    })?;
    write_result.map_err(|error| {
        Error::internal_unexpected(format!(
            "write external storage provider '{}': {error}",
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

fn attach_guard_or_reap(
    child: &mut std::process::Child,
    attach: impl FnOnce(&std::process::Child) -> std::io::Result<()>,
    reap: impl FnOnce(&mut std::process::Child) -> std::io::Result<std::process::ExitStatus>,
) -> std::io::Result<()> {
    if let Err(error) = attach(child) {
        match reap(child) {
            Ok(_) if child.try_wait()?.is_some() => return Err(error),
            Ok(_) => {}
            Err(cleanup_error) => {
                return force_reap_after_attach_failure(child, error, cleanup_error);
            }
        }
        return force_reap_after_attach_failure(
            child,
            error,
            std::io::Error::other("guard cleanup returned without reaping the child"),
        );
    }
    Ok(())
}

fn force_reap_after_attach_failure(
    child: &mut std::process::Child,
    attach_error: std::io::Error,
    cleanup_error: std::io::Error,
) -> std::io::Result<()> {
    let kill_error = child.kill().err();
    let wait_error = child.wait().err();
    let fallback = match (kill_error, wait_error) {
        (_, Some(error)) => format!("fallback reap failed: {error}"),
        (Some(error), None) => format!("fallback kill failed: {error}"),
        (None, None) => "fallback child kill and reap succeeded".to_string(),
    };
    Err(std::io::Error::other(format!(
        "guard attach failed: {attach_error}; process-tree cleanup failed: {cleanup_error}; {fallback}"
    )))
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
        let planned = plan(&inventory, &HashSet::new(), 0, 100, 10, "generation");
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
        assert_eq!(
            plan(&inventory, &HashSet::new(), 0, 100, 1, "generation").len(),
            1
        );
    }

    #[test]
    fn plan_uses_the_serialized_reclaim_prefix_including_opaque_tokens() {
        let mut inventory = (0..MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS + 1)
            .map(|index| {
                item(
                    &format!("item-{index}"),
                    ExternalStorageResourceClass::Scratch,
                )
            })
            .collect::<Vec<_>>();
        for item in &mut inventory {
            item.reclaim_token = "x".repeat(300);
        }
        let planned = plan(
            &inventory,
            &HashSet::new(),
            0,
            u64::MAX,
            usize::MAX,
            "generation",
        );
        assert!(planned.len() < MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS);
        assert!(reclaim_request_fits(
            "generation",
            planned.iter().map(|item| ExternalStorageReclaimTarget {
                id: item.id.clone(),
                reclaim_token: item.reclaim_token.clone(),
            }),
        ));
    }

    #[test]
    fn plan_skips_oversized_candidates_and_keeps_later_fitting_order() {
        let mut oversized = item("oversized", ExternalStorageResourceClass::Scratch);
        oversized.bytes = 100;
        let mut first = item("first-fitting", ExternalStorageResourceClass::Scratch);
        first.bytes = 4;
        let mut second = item("second-fitting", ExternalStorageResourceClass::Scratch);
        second.bytes = 5;
        let inventory = [oversized, first, second];
        let planned = plan(&inventory, &HashSet::new(), 0, 9, 10, "generation");
        assert_eq!(
            planned
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first-fitting", "second-fitting"],
        );
    }

    #[test]
    fn checked_accounting_rejects_u64_overflow() {
        assert!(checked_sum([u64::MAX, 1].into_iter(), "fixture").is_err());
        let first = item("first", ExternalStorageResourceClass::Scratch);
        let mut second = item("second", ExternalStorageResourceClass::Scratch);
        second.bytes = u64::MAX;
        assert_eq!(
            plan(
                &[first, second],
                &HashSet::new(),
                0,
                u64::MAX,
                10,
                "generation"
            )
            .len(),
            1,
        );
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
            plan(&inventory, &HashSet::new(), 7, 10, 10, "generation")
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["old"],
        );
        let pressured = HashSet::from(["root".to_string()]);
        assert_eq!(
            plan(&inventory, &pressured, 7, 20, 10, "generation")
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
    fn inventory_provider(id: &str, item_id: &str) -> ExternalStorageRetentionProviderConfig {
        let inventory = serde_json::json!({
            "schema": EXTERNAL_STORAGE_RETENTION_SCHEMA, "provider_id": id, "generation": "g1",
            "items": [{
                "id": item_id, "root_id": "root", "class": "scratch", "bytes": 10,
                "locator": item_id, "reconstructable": true, "active": false,
                "referenced": false, "ownership_known": true, "age_days": 7,
                "reclaim_token": format!("token-{item_id}")
            }]
        });
        ExternalStorageRetentionProviderConfig {
            id: id.to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("printf '%s' '{}'", inventory),
                "fixture".to_string(),
            ],
            timeout_seconds: 1,
        }
    }

    #[cfg(unix)]
    #[test]
    fn aggregate_limit_is_not_reset_for_each_provider() {
        let output = cleanup_external_storage_with_providers(
            &[
                inventory_provider("one", "one"),
                inventory_provider("two", "two"),
            ],
            ExternalStorageCleanupOptions {
                apply: false,
                min_age_days: 0,
                max_bytes: 10,
                reserve_bytes: 0,
                limit: 1,
                evidence_limit: 10,
                deadline: None,
            },
        )
        .expect("plan");
        assert_eq!(output.candidate_count, 1);
        assert_eq!(output.estimated_bytes, 10);
        assert_eq!(output.provider_count, 1);
        assert_eq!(output.providers.len(), 1);
        assert_eq!(output.providers[0].candidate_count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn aggregate_unknown_byte_overflow_fails_closed() {
        let provider = |id: &str| {
            let inventory = serde_json::json!({
                "schema": EXTERNAL_STORAGE_RETENTION_SCHEMA, "provider_id": id,
                "generation": "g1", "unknown_bytes": u64::MAX,
            });
            ExternalStorageRetentionProviderConfig {
                id: id.to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("printf '%s' '{}'", inventory),
                    "fixture".to_string(),
                ],
                timeout_seconds: 1,
            }
        };
        let result = cleanup_external_storage_with_providers(
            &[provider("one"), provider("two")],
            ExternalStorageCleanupOptions {
                apply: false,
                min_age_days: 0,
                max_bytes: u64::MAX,
                reserve_bytes: 0,
                limit: 10,
                evidence_limit: 1,
                deadline: None,
            },
        );
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn duplicate_inventory_ids_fail_before_reclaim() {
        let receipt = tempfile::NamedTempFile::new().expect("receipt");
        let script = r#"input=$(cat); printf '%s\n' "$input" >> "$1"; printf '%s' '{"schema":"homeboy/external-storage-retention/v1","provider_id":"duplicate","generation":"g1","items":[{"id":"duplicate","root_id":"root","class":"scratch","bytes":1,"locator":"one","reconstructable":true,"active":false,"referenced":false,"ownership_known":true,"age_days":7,"reclaim_token":"one"},{"id":"duplicate","root_id":"root","class":"scratch","bytes":1,"locator":"two","reconstructable":true,"active":false,"referenced":false,"ownership_known":true,"age_days":7,"reclaim_token":"two"}]}'"#;
        let provider = ExternalStorageRetentionProviderConfig {
            id: "duplicate".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                script.to_string(),
                "fixture".to_string(),
                receipt.path().display().to_string(),
            ],
            timeout_seconds: 1,
        };
        let error = cleanup_external_storage_with_providers(
            &[provider],
            ExternalStorageCleanupOptions {
                apply: true,
                min_age_days: 0,
                max_bytes: 10,
                reserve_bytes: 0,
                limit: 10,
                evidence_limit: 1,
                deadline: None,
            },
        )
        .expect_err("duplicate inventory id");
        assert!(error.message.contains("duplicate item id 'duplicate'"));
        let requests = std::fs::read_to_string(receipt.path()).expect("receipt");
        assert!(requests.contains("inventory"));
        assert!(!requests.contains("reclaim"));
    }

    #[cfg(unix)]
    #[test]
    fn blocked_provider_stdin_is_cancelled_by_supervision() {
        let provider = ExternalStorageRetentionProviderConfig {
            id: "blocked".to_string(),
            command: vec!["sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            timeout_seconds: 1,
        };
        let targets = (0..MAX_EXTERNAL_STORAGE_RECLAIM_TARGETS)
            .map(|index| ExternalStorageReclaimTarget {
                id: index.to_string(),
                reclaim_token: "x".repeat(200),
            })
            .collect();
        let started = std::time::Instant::now();
        assert!(invoke_raw(
            &provider,
            ExternalStorageOperation::Reclaim,
            Some("g1".to_string()),
            targets,
            None
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn expired_aggregate_deadline_does_not_spawn_a_provider() {
        let directory = tempfile::tempdir().expect("tempdir");
        let marker = directory.path().join("spawned");
        let provider = ExternalStorageRetentionProviderConfig {
            id: "expired".to_string(),
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("touch '{}'", marker.display()),
            ],
            timeout_seconds: 30,
        };
        let error = invoke_raw(
            &provider,
            ExternalStorageOperation::Inventory,
            None,
            Vec::new(),
            Some(SystemTime::now() - Duration::from_secs(1)),
        )
        .expect_err("expired deadline");
        assert!(error.message.contains("deadline elapsed"));
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn aggregate_deadline_bounds_manual_provider_inventory() {
        let provider = ExternalStorageRetentionProviderConfig {
            id: "manual-deadline".to_string(),
            command: vec!["sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            timeout_seconds: 30,
        };
        let started = std::time::Instant::now();
        assert!(cleanup_external_storage_with_providers(
            &[provider],
            ExternalStorageCleanupOptions {
                apply: false,
                min_age_days: 0,
                max_bytes: 0,
                reserve_bytes: 0,
                limit: 1,
                evidence_limit: 1,
                deadline: Some(SystemTime::now() + Duration::from_millis(1)),
            },
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn guard_attach_failure_reaps_spawned_process() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 5");
        let guard = ControllerChildGuard::prepare(&mut command).expect("guard");
        let mut child = command.spawn().expect("spawn");
        let result = attach_guard_or_reap(
            &mut child,
            |_| Err(std::io::Error::other("fixture attach failure")),
            |_| Err(std::io::Error::other("fixture cleanup failure")),
        );
        assert!(result.is_err());
        assert!(result
            .expect_err("failure")
            .to_string()
            .contains("fixture cleanup failure"));
        assert!(child.try_wait().expect("poll").is_some());
        drop(guard);
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
