//! Immutable caller data shared by local and remote gates.
use super::*;
use homeboy_engine_primitives::content_hash::{
    directory_evidence_entry_record, directory_evidence_tree_digest, sha256_file,
};

#[cfg(test)]
mod tests;

const MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateInput {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    pub env: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub directory: bool,
}

fn io(error: std::io::Error) -> Error {
    Error::internal_io(error.to_string(), None)
}
fn invalid(message: &str) -> Error {
    Error::invalid_argument("gate_input", message)
}

pub fn store_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("HOMEBOY_GATE_INPUT_STORE") {
        return Ok(PathBuf::from(root));
    }
    Ok(homeboy_core::paths::PathRoots::from_environment()?
        .data()
        .join("gate-inputs"))
}

/// Hash every regular file, including media; reject links and special files.
pub fn inspect(path: &Path) -> Result<(String, u64, bool)> {
    fn walk(
        root: &Path,
        path: &Path,
        depth: usize,
        count: &mut usize,
        size: &mut u64,
        records: &mut Vec<(String, String)>,
    ) -> Result<()> {
        *count += 1;
        if *count > 65_536 || depth > 24 {
            return Err(invalid("input exceeds entry or depth limit"));
        }
        let meta = fs::symlink_metadata(path).map_err(io)?;
        if meta.is_dir() {
            let mut children = fs::read_dir(path)
                .map_err(io)?
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(io)?;
            children.sort_by_key(|entry| entry.file_name());
            for entry in children {
                walk(root, &entry.path(), depth + 1, count, size, records)?;
            }
        } else if meta.is_file() {
            *size = size
                .checked_add(meta.len())
                .ok_or_else(|| invalid("input size overflow"))?;
            if *size > MAX_BYTES {
                return Err(invalid("input exceeds 1 GiB"));
            }
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .ok_or_else(|| invalid("input path must be UTF-8"))?
                .to_string();
            if !homeboy_engine_primitives::content_hash::is_safe_evidence_relative_path(&relative) {
                return Err(invalid("unsafe input path"));
            }
            records.push((
                relative.clone(),
                directory_evidence_entry_record(
                    &relative,
                    &format!("sha256:{}", sha256_file(path)?),
                    meta.len(),
                ),
            ));
        } else {
            return Err(invalid(
                "input must contain only regular files and directories",
            ));
        }
        Ok(())
    }
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if meta.is_file() {
        if meta.len() > MAX_BYTES {
            return Err(invalid("input exceeds 1 GiB"));
        }
        return Ok((format!("sha256:{}", sha256_file(path)?), meta.len(), false));
    }
    if !meta.is_dir() {
        return Err(invalid("input must be a regular file or directory"));
    }
    let mut records = Vec::new();
    let mut size = 0;
    walk(path, path, 0, &mut 0, &mut size, &mut records)?;
    records.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((
        directory_evidence_tree_digest(records.iter().map(|r| &r.1)),
        size,
        true,
    ))
}

fn copy(
    source: &Path,
    destination: &Path,
    depth: usize,
    entries: &mut usize,
    bytes: &mut u64,
) -> Result<()> {
    use std::io::Read;
    *entries += 1;
    if *entries > 65_536 || depth > 24 {
        return Err(invalid("input exceeds entry or depth limit"));
    }
    let meta = fs::symlink_metadata(source).map_err(io)?;
    if meta.is_dir() {
        fs::create_dir_all(destination).map_err(io)?;
        for entry in fs::read_dir(source).map_err(io)? {
            let entry = entry.map_err(io)?;
            copy(
                &entry.path(),
                &destination.join(entry.file_name()),
                depth + 1,
                entries,
                bytes,
            )?;
        }
    } else if meta.is_file() {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let input = options.open(source).map_err(io)?;
        if !input.metadata().map_err(io)?.is_file() {
            return Err(invalid("input changed to a special file"));
        }
        let mut output = fs::File::create(destination).map_err(io)?;
        let copied =
            std::io::copy(&mut input.take(MAX_BYTES - *bytes + 1), &mut output).map_err(io)?;
        *bytes += copied;
        if *bytes > MAX_BYTES {
            return Err(invalid("input exceeds 1 GiB"));
        }
    } else {
        return Err(invalid("input changed to a link or special file"));
    }
    Ok(())
}

fn seal(path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(io)? {
            seal(&entry.map_err(io)?.path())?;
        }
    }
    let mut permissions = fs::metadata(path).map_err(io)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).map_err(io)
}

impl GateInput {
    pub fn path_in(&self, root: &Path) -> Result<PathBuf> {
        let hex = self
            .digest
            .strip_prefix("sha256:")
            .filter(|hex| homeboy_engine_primitives::content_hash::is_sha256_hex(hex))
            .ok_or_else(|| invalid("invalid input digest"))?;
        Ok(root
            .join(if self.directory { "trees" } else { "files" })
            .join(hex)
            .join("input"))
    }
    pub fn verify(&self, path: &Path) -> Result<()> {
        if inspect(path)? != (self.digest.clone(), self.size_bytes, self.directory) {
            return Err(invalid("input snapshot digest mismatch"));
        }
        Ok(())
    }
    /// Returns true when an existing verified content-addressed snapshot is reused.
    pub fn materialize(&self, source: &Path, root: &Path) -> Result<bool> {
        let destination = self.path_in(root)?;
        if destination.exists() {
            self.verify(&destination)?;
            return Ok(true);
        }
        let parent = destination.parent().unwrap();
        fs::create_dir_all(parent).map_err(io)?;
        let temp = tempfile::tempdir_in(parent).map_err(io)?;
        let staged = temp.path().join("input");
        copy(source, &staged, 0, &mut 0, &mut 0)?;
        self.verify(&staged)?;
        fs::rename(&staged, &destination).map_err(io)?;
        seal(&destination)?;
        Ok(false)
    }
}

pub fn admit(policy: &mut AgentTaskGateEnvironmentPolicy, root: &Path) -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    let mut envs = std::collections::BTreeSet::new();
    for input in &mut policy.data_inputs {
        if input.id.is_empty() || !ids.insert(input.id.clone()) {
            return Err(invalid("duplicate or empty input id"));
        }
        let valid_env = !input.env.is_empty()
            && input.env.bytes().enumerate().all(|(i, b)| {
                b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
            });
        if !valid_env
            || !envs.insert(input.env.clone())
            || policy.variables.contains_key(&input.env)
            || policy.preserve.contains_key(&input.env)
        {
            return Err(invalid("invalid, duplicate, or colliding input env"));
        }
        if input.source.is_empty() {
            input.verify(&input.path_in(root)?)?;
            continue;
        }
        let source = Path::new(&input.source);
        if !source.is_absolute() {
            return Err(invalid("input source must be absolute"));
        }
        let (digest, size_bytes, directory) = inspect(source)?;
        if !input.digest.is_empty()
            && (input.digest != digest
                || input.size_bytes != size_bytes
                || input.directory != directory)
        {
            return Err(invalid("input source differs from admitted digest"));
        }
        input.digest = digest;
        input.size_bytes = size_bytes;
        input.directory = directory;
        input.materialize(source, root)?;
        input.source.clear();
    }
    Ok(())
}
