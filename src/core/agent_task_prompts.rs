use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::core::{paths, Error, Result};

pub const PROMPT_REF_PREFIX: &str = "prompt:";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskPromptRecord {
    pub id: String,
    pub path: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_unix: Option<u64>,
}

pub fn prompts_dir() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task").join("prompts"))
}

pub fn prompt_id(name: &str) -> Result<String> {
    let trimmed = name.trim().trim_end_matches(".md");
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            "name",
            "prompt name cannot be empty",
            None,
            None,
        ));
    }

    let id = paths::sanitize_path_segment(trimmed)
        .trim_matches('_')
        .to_string();
    if id.is_empty() || id == "." || id == ".." {
        return Err(Error::validation_invalid_argument(
            "name",
            "prompt name must include at least one safe path character",
            Some(name.to_string()),
            None,
        ));
    }

    Ok(id)
}

pub fn prompt_path(name: &str) -> Result<PathBuf> {
    Ok(prompts_dir()?.join(format!("{}.md", prompt_id(name)?)))
}

pub fn save_prompt(name: &str, content: &str) -> Result<AgentTaskPromptRecord> {
    let path = prompt_path(name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    fs::write(&path, content)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    prompt_record_for_path(prompt_id(name)?, path)
}

pub fn read_prompt(name: &str) -> Result<String> {
    let path = prompt_path(name)?;
    fs::read_to_string(&path).map_err(|error| prompt_io_error("prompt", name, &path, error))
}

pub fn remove_prompt(name: &str) -> Result<AgentTaskPromptRecord> {
    let id = prompt_id(name)?;
    let path = prompt_path(&id)?;
    let record = prompt_record_for_path(id, path.clone())?;
    fs::remove_file(&path).map_err(|error| prompt_io_error("prompt", name, &path, error))?;
    Ok(record)
}

pub fn list_prompts() -> Result<Vec<AgentTaskPromptRecord>> {
    let dir = prompts_dir()?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(dir.display().to_string()),
            ))
        }
    };

    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(dir.display().to_string()))
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        records.push(prompt_record_for_path(stem.to_string(), path)?);
    }
    records.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(records)
}

pub fn resolve_stored_prompt_ref(spec: &str) -> Result<Option<String>> {
    let Some(name) = spec.strip_prefix(PROMPT_REF_PREFIX) else {
        return Ok(None);
    };
    Ok(Some(read_prompt(name)?))
}

pub fn read_prompt_input(spec: &str) -> Result<String> {
    if spec.trim() == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|error| {
            Error::internal_io(error.to_string(), Some("read stdin".to_string()))
        })?;
        return Ok(buf);
    }

    if let Some(path) = spec.strip_prefix('@') {
        if path.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "input",
                "Invalid prompt input '@' (missing file path)",
                None,
                None,
            ));
        }
        return fs::read_to_string(path)
            .map_err(|error| Error::internal_io(error.to_string(), Some(path.to_string())));
    }

    Ok(spec.to_string())
}

fn prompt_record_for_path(id: String, path: PathBuf) -> Result<AgentTaskPromptRecord> {
    let metadata = fs::metadata(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    Ok(AgentTaskPromptRecord {
        id,
        path: path.display().to_string(),
        size_bytes: metadata.len(),
        modified_unix,
    })
}

fn prompt_io_error(
    field: &str,
    name: &str,
    path: &std::path::Path,
    error: std::io::Error,
) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        return Error::validation_invalid_argument(
            field,
            format!("stored agent-task prompt '{}' was not found", name),
            Some(name.to_string()),
            Some(vec![
                "Run `homeboy agent-task prompts list` to see stored prompts".to_string(),
            ]),
        );
    }
    Error::internal_io(error.to_string(), Some(path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn stores_markdown_prompts_under_homeboy_data() {
        with_isolated_home(|home| {
            let record =
                save_prompt("Cook: PR 123.md", "# Cook\nDo the thing.\n").expect("saved prompt");

            assert_eq!(record.id, "Cook__PR_123");
            assert!(record.path.ends_with("agent-task/prompts/Cook__PR_123.md"));
            assert!(record.path.starts_with(&home.path().display().to_string()));
            assert_eq!(
                read_prompt("Cook__PR_123").expect("read prompt"),
                "# Cook\nDo the thing.\n"
            );
        });
    }

    #[test]
    fn lists_and_resolves_prompt_refs() {
        with_isolated_home(|_| {
            save_prompt("beta", "second").expect("save beta");
            save_prompt("alpha", "first").expect("save alpha");

            let ids: Vec<String> = list_prompts()
                .expect("list prompts")
                .into_iter()
                .map(|record| record.id)
                .collect();
            assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
            assert_eq!(
                resolve_stored_prompt_ref("prompt:alpha").expect("resolve ref"),
                Some("first".to_string())
            );
            assert_eq!(
                resolve_stored_prompt_ref("plain prompt").expect("plain prompt"),
                None
            );
        });
    }
}
