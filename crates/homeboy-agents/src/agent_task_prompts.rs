use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use homeboy_core::{paths, Error, Result};

pub const PROMPT_REF_PREFIX: &str = "prompt:";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentTaskPromptRecord {
    pub id: String,
    pub path: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_unix: Option<u64>,
}

struct PromptStore {
    dir: PathBuf,
}

impl PromptStore {
    fn from_data_root(data_root: PathBuf) -> Self {
        Self {
            dir: data_root.join("agent-task").join("prompts"),
        }
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        Ok(self.dir.join(format!("{}.md", prompt_id(name)?)))
    }

    fn save(&self, name: &str, content: &str) -> Result<AgentTaskPromptRecord> {
        let path = self.path(name)?;
        fs::create_dir_all(&self.dir).map_err(|error| {
            Error::internal_io(error.to_string(), Some(self.dir.display().to_string()))
        })?;
        fs::write(&path, content).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        prompt_record_for_path(prompt_id(name)?, path)
    }

    fn read(&self, name: &str) -> Result<String> {
        let path = self.path(name)?;
        fs::read_to_string(&path).map_err(|error| self.io_error("prompt", name, &path, error))
    }

    fn remove(&self, name: &str) -> Result<AgentTaskPromptRecord> {
        let id = prompt_id(name)?;
        let path = self.path(&id)?;
        let record = prompt_record_for_path(id, path.clone())?;
        fs::remove_file(&path).map_err(|error| self.io_error("prompt", name, &path, error))?;
        Ok(record)
    }

    fn list(&self) -> Result<Vec<AgentTaskPromptRecord>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(self.dir.display().to_string()),
                ))
            }
        };

        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                Error::internal_io(error.to_string(), Some(self.dir.display().to_string()))
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

    fn resolve(&self, spec: &str) -> Result<Option<String>> {
        let Some(name) = stored_prompt_ref_id(spec) else {
            return Ok(None);
        };
        Ok(Some(self.read(name)?))
    }

    fn io_error(
        &self,
        field: &str,
        name: &str,
        path: &std::path::Path,
        error: std::io::Error,
    ) -> Error {
        if error.kind() == std::io::ErrorKind::NotFound {
            let mut hints =
                vec!["Run `homeboy agent-task prompts list` to see stored prompts".to_string()];
            if let Ok(records) = self.list() {
                let ids = records
                    .into_iter()
                    .map(|record| record.id)
                    .collect::<Vec<_>>();
                if !ids.is_empty() {
                    hints.push(format!("Available stored prompt ids: {}", ids.join(", ")));
                }
            }
            return Error::validation_invalid_argument(
                field,
                format!("stored agent-task prompt '{}' was not found", name),
                Some(name.to_string()),
                Some(hints),
            );
        }
        Error::internal_io(error.to_string(), Some(path.display().to_string()))
    }
}

fn prompt_store() -> Result<PromptStore> {
    Ok(PromptStore::from_data_root(paths::homeboy_data()?))
}

pub fn prompts_dir() -> Result<PathBuf> {
    Ok(prompt_store()?.dir)
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
    prompt_store()?.path(name)
}

pub fn save_prompt(name: &str, content: &str) -> Result<AgentTaskPromptRecord> {
    prompt_store()?.save(name, content)
}

pub fn read_prompt(name: &str) -> Result<String> {
    prompt_store()?.read(name)
}

pub fn remove_prompt(name: &str) -> Result<AgentTaskPromptRecord> {
    prompt_store()?.remove(name)
}

pub fn list_prompts() -> Result<Vec<AgentTaskPromptRecord>> {
    prompt_store()?.list()
}

pub fn resolve_stored_prompt_ref(spec: &str) -> Result<Option<String>> {
    prompt_store()?.resolve(spec)
}

pub fn stored_prompt_ref_id(spec: &str) -> Option<&str> {
    spec.strip_prefix('@')
        .unwrap_or(spec)
        .strip_prefix(PROMPT_REF_PREFIX)
}

pub fn read_prompt_input(spec: &str) -> Result<String> {
    if spec.trim() == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|error| {
            Error::internal_io(error.to_string(), Some("read stdin".to_string()))
        })?;
        return Ok(buf);
    }

    if let Some(prompt) = resolve_stored_prompt_ref(spec)? {
        return Ok(prompt);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_markdown_prompts_under_configured_data_root() {
        let temp = tempfile::tempdir().expect("prompt store");
        let store = PromptStore::from_data_root(temp.path().to_path_buf());
        let record = store
            .save("Cook: PR 123.md", "# Cook\nDo the thing.\n")
            .expect("saved prompt");

        assert_eq!(record.id, "Cook__PR_123");
        assert!(record.path.ends_with("agent-task/prompts/Cook__PR_123.md"));
        assert!(record.path.starts_with(&temp.path().display().to_string()));
        assert_eq!(
            store.read("Cook__PR_123").expect("read prompt"),
            "# Cook\nDo the thing.\n"
        );
    }

    #[test]
    fn lists_and_resolves_prompt_refs() {
        let temp = tempfile::tempdir().expect("prompt store");
        let store = PromptStore::from_data_root(temp.path().to_path_buf());
        store.save("beta", "second").expect("save beta");
        store.save("alpha", "first").expect("save alpha");

        let ids: Vec<String> = store
            .list()
            .expect("list prompts")
            .into_iter()
            .map(|record| record.id)
            .collect();
        assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(
            store.resolve("prompt:alpha").expect("resolve ref"),
            Some("first".to_string())
        );
        assert_eq!(
            store.resolve("@prompt:alpha").expect("resolve at ref"),
            Some("first".to_string())
        );
        assert_eq!(store.resolve("plain prompt").expect("plain prompt"), None);
    }
}
