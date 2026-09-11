use std::fs;
use std::io::Write;
use std::path::PathBuf;

use homeboy_engine_primitives::local_files;
use homeboy_extension_contract::api::v1::ExtensionApiExecuteResponse;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const IDEMPOTENCY_RECORD_SCHEMA: &str = "homeboy/extension-api-execute-idempotency/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IdempotencyRecordState {
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IdempotencyRecord {
    schema: String,
    fingerprint: String,
    state: IdempotencyRecordState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response: Option<ExtensionApiExecuteResponse>,
}

pub(super) enum IdempotencyClaim {
    Accepted,
    Replayed(ExtensionApiExecuteResponse),
    InProgress,
    Conflict,
}

pub(super) fn claim(
    idempotency_key: &str,
    fingerprint: &serde_json::Value,
) -> Result<IdempotencyClaim, String> {
    let path = record_path(idempotency_key)?;
    let fingerprint = fingerprint_digest(fingerprint)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    match exclusive_create(&path) {
        Ok(mut file) => {
            let record = IdempotencyRecord {
                schema: IDEMPOTENCY_RECORD_SCHEMA.to_string(),
                fingerprint,
                state: IdempotencyRecordState::InProgress,
                response: None,
            };
            let serialized =
                serde_json::to_string_pretty(&record).map_err(|error| error.to_string())?;
            file.write_all(serialized.as_bytes())
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            Ok(IdempotencyClaim::Accepted)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(existing_claim(&path, &fingerprint))
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn complete(
    idempotency_key: &str,
    fingerprint: &serde_json::Value,
    response: &ExtensionApiExecuteResponse,
) -> Result<(), String> {
    let path = record_path(idempotency_key)?;
    let record = IdempotencyRecord {
        schema: IDEMPOTENCY_RECORD_SCHEMA.to_string(),
        fingerprint: fingerprint_digest(fingerprint)?,
        state: IdempotencyRecordState::Completed,
        response: Some(response.clone()),
    };
    let serialized = serde_json::to_string_pretty(&record).map_err(|error| error.to_string())?;
    local_files::write_file_atomic(&path, &serialized, "write extension execute idempotency")
        .map_err(|error| error.to_string())
}

fn existing_claim(path: &std::path::Path, fingerprint: &str) -> IdempotencyClaim {
    let Ok(contents) = fs::read_to_string(path) else {
        return IdempotencyClaim::InProgress;
    };
    let Ok(record) = serde_json::from_str::<IdempotencyRecord>(&contents) else {
        return IdempotencyClaim::InProgress;
    };
    if record.fingerprint != fingerprint {
        return IdempotencyClaim::Conflict;
    }
    match (record.state, record.response) {
        (IdempotencyRecordState::Completed, Some(response)) => IdempotencyClaim::Replayed(response),
        _ => IdempotencyClaim::InProgress,
    }
}

fn fingerprint_digest(fingerprint: &serde_json::Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(fingerprint).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn exclusive_create(path: &std::path::Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

fn record_path(idempotency_key: &str) -> Result<PathBuf, String> {
    let roots =
        homeboy_core::paths::PathRoots::from_environment().map_err(|error| error.to_string())?;
    let digest = Sha256::digest(idempotency_key.as_bytes());
    Ok(roots
        .data()
        .join("extension-execute-idempotency")
        .join(format!("{digest:x}.json")))
}
