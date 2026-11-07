//! JSON sidecar file mechanics behind `sidecar-writer.sh`.
//!
//! Every write lands atomically: the document is written to a
//! `.homeboy-sidecar-*.json` temp file beside the target and renamed over it,
//! so a reader never observes a partial sidecar. Output is compact UTF-8 with
//! a trailing newline.

use super::json::Json;
use std::fs;
use std::io::Write;
use std::path::Path;

/// Replace `target` with a JSON array of `items`.
pub fn write_array(target: &str, items: &[String]) -> Result<(), String> {
    if target.is_empty() {
        return Ok(());
    }
    let items = items
        .iter()
        .map(|raw| Json::parse(raw))
        .collect::<Result<Vec<_>, _>>()?;
    write_atomic(target, &Json::Array(items))
}

/// Replace `target` with a single JSON object.
pub fn write_object(target: &str, item: &str) -> Result<(), String> {
    if target.is_empty() {
        return Ok(());
    }
    let item = Json::parse(item)?;
    if !matches!(item, Json::Object(_)) {
        return Err("sidecar must contain a JSON object".to_string());
    }
    write_atomic(target, &item)
}

/// Append one JSON value to the array in `target`, creating it if absent.
pub fn append(target: &str, item: &str) -> Result<(), String> {
    if target.is_empty() {
        return Ok(());
    }
    let item = Json::parse(item)?;
    let mut items = read_array(target)?;
    items.push(item);
    write_atomic(target, &Json::Array(items))
}

/// Extend the array in `target` with the array stored in `source`.
/// A missing or empty source is a no-op.
pub fn merge(target: &str, source: &str) -> Result<(), String> {
    let source_is_empty = fs::metadata(source).map_or(true, |meta| meta.len() == 0);
    if target.is_empty() || source.is_empty() || source_is_empty {
        return Ok(());
    }
    let mut items = read_array(target)?;
    let raw = fs::read_to_string(source).map_err(|error| format!("read {source}: {error}"))?;
    match Json::parse(&raw)? {
        Json::Array(incoming) => items.extend(incoming),
        _ => {
            return Err(format!(
                "source sidecar must contain a JSON array: {source}"
            ))
        }
    }
    write_atomic(target, &Json::Array(items))
}

fn read_array(target: &str) -> Result<Vec<Json>, String> {
    let Ok(meta) = fs::metadata(target) else {
        return Ok(Vec::new());
    };
    if meta.len() == 0 {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(target).map_err(|error| format!("read {target}: {error}"))?;
    match Json::parse(&raw)? {
        Json::Array(items) => Ok(items),
        _ => Err(format!("sidecar must contain a JSON array: {target}")),
    }
}

fn write_atomic(target: &str, value: &Json) -> Result<(), String> {
    let target = Path::new(target);
    let directory = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".homeboy-sidecar-")
        .suffix(".json")
        .tempfile_in(directory)
        .map_err(|error| format!("create temp sidecar in {}: {error}", directory.display()))?;
    let mut text = value.to_compact();
    text.push('\n');
    temp.write_all(text.as_bytes())
        .and_then(|()| temp.flush())
        .map_err(|error| format!("write temp sidecar: {error}"))?;
    temp.persist(target)
        .map_err(|error| format!("replace {}: {}", target.display(), error.error))?;
    Ok(())
}
