//! Runner-portable staging of provider agent directories.
//!
//! A provider agent directory (e.g. Pi's `PI_CODING_AGENT_DIR`) referenced by
//! provider config is staged from the controller onto the Lab runner. Only its
//! portable configuration (models/settings JSON, prompts, auth references, ...)
//! belongs on the runner. Controller-platform executables — notably a provider
//! tool cache `bin/` holding a Mach-O `rg` — fail on a runner of a different
//! platform with `Exec format error`, so they are omitted and the runner
//! resolves (or downloads) its own tools instead.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde::Serialize;

use homeboy_core::{Error, Result};

pub(crate) const PROVIDER_AGENT_DIR_STAGING_SCHEMA: &str = "homeboy/provider-agent-dir-staging/v1";

/// Receipt for one staged provider agent directory: which controller entries
/// were withheld from the runner because they are not runner-portable.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderAgentDirStagingReceipt {
    pub(crate) schema: String,
    /// Relative paths (directories end with `/`) omitted from staging.
    pub(crate) omitted: Vec<String>,
    pub(crate) omitted_count: usize,
}

impl ProviderAgentDirStagingReceipt {
    /// Root-anchored snapshot excludes realizing the omissions.
    pub(crate) fn snapshot_excludes(&self) -> Vec<String> {
        self.omitted
            .iter()
            .map(|path| match path.strip_suffix('/') {
                Some(dir) => format!("./{dir}/**"),
                None => format!("./{path}"),
            })
            .collect()
    }
}

/// Plan the staging of a provider agent directory: walk it (without following
/// links) and record every entry that is not runner-portable.
pub(crate) fn plan_provider_agent_dir_staging(
    root: &Path,
) -> Result<ProviderAgentDirStagingReceipt> {
    let mut omitted = Vec::new();
    collect_unportable(root, root, &mut omitted)?;
    omitted.sort();
    Ok(ProviderAgentDirStagingReceipt {
        schema: PROVIDER_AGENT_DIR_STAGING_SCHEMA.to_string(),
        omitted_count: omitted.len(),
        omitted,
    })
}

fn collect_unportable(root: &Path, dir: &Path, omitted: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read provider agent dir {}", dir.display())),
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if metadata.is_dir() {
            if is_runner_unportable(&path, &metadata) {
                omitted.push(format!("{rel}/"));
            } else {
                collect_unportable(root, &path, omitted)?;
            }
        } else if metadata.is_file() && is_runner_unportable(&path, &metadata) {
            omitted.push(rel);
        }
    }
    Ok(())
}

/// The single staging predicate: a `bin/` directory, or a regular file that
/// carries native binary magic, is controller-platform specific and must not
/// be staged onto a runner that may differ. Scripts stay: they are portable.
pub(crate) fn is_runner_unportable(path: &Path, metadata: &fs::Metadata) -> bool {
    if metadata.is_dir() {
        return path.file_name().and_then(|name| name.to_str()) == Some("bin");
    }
    if !metadata.is_file() {
        return false;
    }
    has_native_binary_magic(path)
}

fn has_native_binary_magic(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut read = 0;
    while read < magic.len() {
        match file.read(&mut magic[read..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    is_native_binary_magic(&magic[..read])
}

pub(crate) fn is_native_binary_magic(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"MZ") || bytes.starts_with(b"\x7fELF") {
        return true;
    }
    let Some(head) = bytes.get(..4) else {
        return false;
    };
    matches!(
        head,
        // Mach-O 32/64-bit, both byte orders, and universal (fat) binaries.
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
    )
}
