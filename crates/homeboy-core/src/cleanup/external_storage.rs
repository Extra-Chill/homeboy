//! Generic execution and policy for extension-owned external storage.
//!
//! The extension provider is the only code that understands its roots and its
//! durable store. Core validates inventory, applies lifecycle policy, and sends
//! selected identities back for native reclaim. It intentionally never calls
//! `remove_dir_all` on a provider locator.

use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use homeboy_extension_contract::{
    ExternalStorageInventory, ExternalStorageItem, ExternalStorageOperation,
    ExternalStorageReclaimResult, ExternalStorageRequest, ExternalStorageResourceClass,
    ExternalStorageRetentionProviderConfig, EXTERNAL_STORAGE_RETENTION_SCHEMA,
};
use serde::Serialize;

use crate::{Error, Result};

const PROVIDER_OUTPUT_LIMIT: usize = 1024 * 1024;
const PROVIDER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct ExternalStorageCleanupOptions {
    pub apply: bool,
    pub min_age_days: u64,
    pub limit: usize,
    pub evidence_limit: usize,
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
        let inventory = invoke(provider, ExternalStorageOperation::Inventory, Vec::new())?;
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
        let candidates = plan(&inventory.items, options.min_age_days, options.limit);
        let estimated_bytes = candidates.iter().map(|item| item.bytes).sum();
        let item_ids = candidates.iter().map(|item| item.id.clone()).collect();
        let reclaimed_bytes = if options.apply && !candidates.is_empty() {
            // A provider performs deletion/compaction atomically in its own
            // model. Core's candidate bytes are accounting, not path authority.
            let reclaim = invoke_reclaim(provider, item_ids)?;
            if reclaim.schema != EXTERNAL_STORAGE_RETENTION_SCHEMA
                || reclaim.provider_id != provider.id
            {
                return Err(Error::internal_unexpected(
                    "external storage provider returned an invalid reclaim response",
                ));
            }
            reclaim.reclaimed_bytes
        } else {
            0
        };
        let evidence = candidates
            .iter()
            .take(options.evidence_limit)
            .map(|item| evidence(item))
            .collect();
        let provider_output = ExternalStorageProviderOutput {
            provider_id: provider.id.clone(),
            candidate_count: candidates.len(),
            applied_count: if options.apply { candidates.len() } else { 0 },
            skipped_count: inventory.items.len().saturating_sub(candidates.len()),
            estimated_bytes,
            reclaimed_bytes,
            unknown_bytes: inventory.unknown_bytes,
            candidates: evidence,
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

fn plan(
    items: &[ExternalStorageItem],
    min_age_days: u64,
    limit: usize,
) -> Vec<&ExternalStorageItem> {
    items
        .iter()
        .filter(|item| item.ownership_known)
        .filter(|item| item.reconstructable)
        .filter(|item| !item.active && !item.referenced)
        .filter(|item| {
            !matches!(
                item.class,
                ExternalStorageResourceClass::Credential
                    | ExternalStorageResourceClass::PinnedExport
            )
        })
        .filter(|item| item.age_days >= min_age_days)
        .take(limit)
        .collect()
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
    item_ids: Vec<String>,
) -> Result<ExternalStorageInventory> {
    let value = invoke_raw(provider, operation, item_ids)?;
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
    item_ids: Vec<String>,
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
        item_ids,
    })
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize external storage request".to_string()),
        )
    })?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
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
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(provider.timeout_seconds))
        .unwrap_or_else(Instant::now);
    loop {
        if child
            .try_wait()
            .map_err(|error| {
                Error::internal_unexpected(format!(
                    "poll external storage provider '{}': {error}",
                    provider.id
                ))
            })?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::internal_unexpected(format!(
                "external storage provider '{}' exceeded its timeout",
                provider.id
            )));
        }
        std::thread::sleep(PROVIDER_POLL_INTERVAL);
    }
    let result = child.wait_with_output().map_err(|error| {
        Error::internal_unexpected(format!(
            "wait for external storage provider '{}': {error}",
            provider.id
        ))
    })?;
    if !result.status.success() || result.stdout.len() > PROVIDER_OUTPUT_LIMIT {
        return Err(Error::internal_unexpected(format!(
            "external storage provider '{}' failed or exceeded its output budget",
            provider.id
        )));
    }
    Ok(result.stdout)
}

fn invoke_reclaim(
    provider: &ExternalStorageRetentionProviderConfig,
    item_ids: Vec<String>,
) -> Result<ExternalStorageReclaimResult> {
    let value = invoke_raw(provider, ExternalStorageOperation::Reclaim, item_ids)?;
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
            class,
            bytes: 10,
            locator: id.to_string(),
            reconstructable: true,
            active: false,
            referenced: false,
            ownership_known: true,
            age_days: 7,
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
        let planned = plan(&inventory, 0, 10);
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
        assert_eq!(plan(&inventory, 0, 1).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn provider_inventory_is_planned_and_reclaimed_without_path_deletion() {
        let receipt = tempfile::NamedTempFile::new().expect("receipt");
        let script = r#"input=$(cat); printf '%s' "$input" > "$1"; case "$input" in *'reclaim'*) printf '%s' '{"schema":"homeboy/external-storage-retention/v1","provider_id":"fixture","reclaimed_item_ids":["scratch"],"reclaimed_bytes":12}' ;; *) printf '%s' '{"schema":"homeboy/external-storage-retention/v1","provider_id":"fixture","unknown_bytes":99,"items":[{"id":"scratch","class":"scratch","bytes":12,"locator":"external/tmp/scratch","reconstructable":true,"active":false,"referenced":false,"ownership_known":true,"age_days":7},{"id":"live-db","class":"session_store","bytes":58,"locator":"external/data/live.db","reconstructable":false,"active":true,"referenced":true,"ownership_known":true,"age_days":7}]}' ;; esac"#;
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
                limit: 10,
                evidence_limit: 1,
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
