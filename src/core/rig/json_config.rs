use crate::core::error::{Error, Result};
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct JsonConfigEntry {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ParsedJsonConfig<T> {
    pub(crate) id: String,
    pub(crate) value: T,
}

#[derive(Debug)]
pub(crate) struct InvalidJsonConfig {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) error: String,
}

#[derive(Debug)]
pub(crate) struct ParsedJsonConfigs<T> {
    pub(crate) valid: Vec<ParsedJsonConfig<T>>,
    pub(crate) invalid: Vec<InvalidJsonConfig>,
}

pub(crate) fn sorted_json_config_entries(
    dir: &Path,
    read_dir_context: &'static str,
    read_entry_context: &'static str,
    map_error: impl Fn(std::io::Error, &'static str) -> Error,
) -> Result<Vec<JsonConfigEntry>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| map_error(e, read_dir_context))? {
        let entry = entry.map_err(|e| map_error(e, read_entry_context))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        entries.push(JsonConfigEntry {
            id: id.to_string(),
            path,
        });
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

pub(crate) fn read_json_configs<T>(
    dir: &Path,
    read_dir_context: &'static str,
    read_entry_context: &'static str,
) -> Result<ParsedJsonConfigs<T>>
where
    T: DeserializeOwned,
{
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for entry in
        sorted_json_config_entries(dir, read_dir_context, read_entry_context, |e, context| {
            Error::internal_io(e.to_string(), Some(context.into()))
        })?
    {
        let content = match fs::read_to_string(&entry.path) {
            Ok(content) => content,
            Err(err) => {
                invalid.push(InvalidJsonConfig {
                    id: entry.id,
                    path: entry.path,
                    error: err.to_string(),
                });
                continue;
            }
        };

        match serde_json::from_str::<T>(&content) {
            Ok(value) => valid.push(ParsedJsonConfig {
                id: entry.id,
                value,
            }),
            Err(err) => invalid.push(InvalidJsonConfig {
                id: entry.id,
                path: entry.path,
                error: err.to_string(),
            }),
        }
    }
    invalid.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(ParsedJsonConfigs { valid, invalid })
}
