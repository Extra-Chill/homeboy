//! Filesystem parsers and inventory merge helpers for fuzz contracts.

use std::path::Path;

use serde_json::Value;

use crate::core::{Error, Result};

use super::envelope::{FuzzResultEnvelope, FuzzTargetInventory};
use super::schemas::{FUZZ_CAMPAIGN_SCHEMA, FUZZ_RESULT_ENVELOPE_SCHEMA};
use super::types::{
    FuzzActionModel, FuzzCampaign, FuzzCaseLogEntry, FuzzExplorationPolicy, FuzzSequencePlan,
    FuzzWorkload,
};

pub fn parse_fuzz_results_file(path: &Path) -> Result<FuzzCampaign> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let campaign: FuzzCampaign = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse fuzz results file {}", path.display())),
            Some(contents),
        )
    })?;
    if campaign.schema != FUZZ_CAMPAIGN_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "fuzz results schema must be {FUZZ_CAMPAIGN_SCHEMA}, got {}",
                campaign.schema
            ),
            Some(campaign.schema),
            None,
        ));
    }
    Ok(campaign)
}

pub fn parse_fuzz_case_log_file(path: &Path) -> Result<Vec<FuzzCaseLogEntry>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let entries = parse_fuzz_case_log_contents(&contents).map_err(|message| {
        Error::validation_invalid_argument(
            "case_log",
            message,
            Some(path.display().to_string()),
            None,
        )
    })?;
    Ok(entries)
}

pub fn parse_fuzz_case_log_contents(
    contents: &str,
) -> std::result::Result<Vec<FuzzCaseLogEntry>, String> {
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return Err("case log must contain at least one entry".to_string());
    }

    let values = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<Value>>(trimmed).map_err(|err| err.to_string())?
    } else if trimmed.starts_with('{') {
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => match value.get("entries").and_then(Value::as_array) {
                Some(entries) => entries.clone(),
                None => vec![value],
            },
            Err(_) if trimmed.lines().count() > 1 => parse_fuzz_case_log_jsonl(trimmed)?,
            Err(error) => return Err(error.to_string()),
        }
    } else {
        parse_fuzz_case_log_jsonl(trimmed)?
    };

    if values.is_empty() {
        return Err("case log must contain at least one entry".to_string());
    }

    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let mut entry: FuzzCaseLogEntry = serde_json::from_value(value)
                .map_err(|err| format!("case log entry {}: {err}", index + 1))?;
            entry
                .normalize()
                .map_err(|err| format!("case log entry {}: {err}", index + 1))?;
            Ok(entry)
        })
        .collect()
}

fn parse_fuzz_case_log_jsonl(contents: &str) -> std::result::Result<Vec<Value>, String> {
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line.trim())
                .map_err(|err| format!("case log line {}: {err}", index + 1))
        })
        .collect()
}

pub fn parse_fuzz_result_envelope_file(path: &Path) -> Result<FuzzResultEnvelope> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let envelope: FuzzResultEnvelope = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!(
                "parse fuzz result envelope file {}",
                path.display()
            )),
            Some(contents),
        )
    })?;
    if envelope.schema != FUZZ_RESULT_ENVELOPE_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "schema",
            format!(
                "fuzz result envelope schema must be {FUZZ_RESULT_ENVELOPE_SCHEMA}, got {}",
                envelope.schema
            ),
            Some(envelope.schema),
            None,
        ));
    }
    Ok(envelope)
}

pub fn parse_fuzz_target_inventory_file(path: &Path) -> Result<FuzzTargetInventory> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let value: Value = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!(
                "parse fuzz target inventory file {}",
                path.display()
            )),
            Some(contents.clone()),
        )
    })?;
    FuzzTargetInventory::from_value(value).map_err(|message| {
        Error::validation_invalid_argument(
            "inventory",
            message,
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn parse_fuzz_action_model_file(path: &Path) -> Result<FuzzActionModel> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let value: Value = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse fuzz action model file {}", path.display())),
            Some(contents.clone()),
        )
    })?;
    FuzzActionModel::from_value(value).map_err(|message| {
        Error::validation_invalid_argument(
            "action_model",
            message,
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn parse_fuzz_workload_file(path: &Path) -> Result<FuzzWorkload> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let value: Value = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse fuzz workload file {}", path.display())),
            Some(contents.clone()),
        )
    })?;
    FuzzWorkload::from_value(value).map_err(|message| {
        Error::validation_invalid_argument(
            "workload",
            message,
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn parse_fuzz_exploration_policy_file(path: &Path) -> Result<FuzzExplorationPolicy> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let value: Value = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!(
                "parse fuzz exploration policy file {}",
                path.display()
            )),
            Some(contents.clone()),
        )
    })?;
    FuzzExplorationPolicy::from_value(value).map_err(|message| {
        Error::validation_invalid_argument(
            "exploration_policy",
            message,
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn parse_fuzz_sequence_plan_file(path: &Path) -> Result<FuzzSequencePlan> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let value: Value = serde_json::from_str(&contents).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some(format!("parse fuzz sequence plan file {}", path.display())),
            Some(contents.clone()),
        )
    })?;
    FuzzSequencePlan::from_value(value).map_err(|message| {
        Error::validation_invalid_argument(
            "sequence_plan",
            message,
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn merge_fuzz_target_inventory(
    base: &mut FuzzTargetInventory,
    mut discovered: FuzzTargetInventory,
) {
    base.surfaces.append(&mut discovered.surfaces);
    base.targets.append(&mut discovered.targets);
    base.workloads.append(&mut discovered.workloads);
    base.seeds.append(&mut discovered.seeds);
    if base.provenance.is_none() {
        base.provenance = discovered.provenance;
    }
    merge_metadata(&mut base.metadata, discovered.metadata);
    base.extra.append(&mut discovered.extra);
}

fn merge_metadata(base: &mut Value, discovered: Value) {
    if discovered.is_null() {
        return;
    }
    if base.is_null() {
        *base = discovered;
        return;
    }
    match (base, discovered) {
        (Value::Object(base_map), Value::Object(incoming_map)) => {
            for (key, value) in incoming_map {
                base_map.entry(key).or_insert(value);
            }
        }
        (base, incoming) => {
            let previous = std::mem::take(base);
            *base = serde_json::json!({
                "homeboy_metadata": previous,
                "merged_inventory_metadata": incoming,
            });
        }
    }
}
