//! Generic `@file` argument support for Lab offload.
//!
//! Homeboy commands commonly accept `@relative/path` as a local file spec. Git
//! Lab workspaces are clean checkouts, so generated ignored/untracked files must
//! be materialized explicitly before the runner command executes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::core::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core::runner) struct LabAtFileSpec {
    pub(in crate::core::runner) original_spec: String,
    pub(in crate::core::runner) local_path: PathBuf,
    pub(in crate::core::runner) remote_spec: String,
    pub(in crate::core::runner) remote_path: String,
}

pub(in crate::core::runner) fn lab_at_file_specs(
    args: &[String],
    source_path: &Path,
    remote_cwd: &str,
) -> Result<Vec<LabAtFileSpec>> {
    let mut specs = Vec::new();
    let mut seen = BTreeSet::new();
    let cwd = std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("read cwd".to_string())))?;

    for spec in at_file_tokens(args) {
        let resolved = resolve_at_file_spec(&spec, &cwd, source_path, remote_cwd)?;
        let key = (
            resolved.original_spec.clone(),
            resolved.local_path.display().to_string(),
            resolved.remote_path.clone(),
        );
        if seen.insert(key) {
            specs.push(resolved);
        }
    }

    Ok(specs)
}

pub(in crate::core::runner) fn remap_lab_at_file_args(
    args: &[String],
    specs: &[LabAtFileSpec],
) -> Vec<String> {
    if specs.is_empty() {
        return args.to_vec();
    }

    args.iter()
        .map(|arg| {
            for spec in specs {
                if arg == &spec.original_spec {
                    return spec.remote_spec.clone();
                }
                if let Some((prefix, value)) = arg.split_once('=') {
                    if value == spec.original_spec {
                        return format!("{prefix}={}", spec.remote_spec);
                    }
                }
            }
            arg.clone()
        })
        .collect()
}

fn at_file_tokens(args: &[String]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut passthrough = false;

    for arg in args {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        if arg.starts_with('@') {
            tokens.push(arg.clone());
            continue;
        }
        if let Some((_, value)) = arg.split_once('=') {
            if value.starts_with('@') {
                tokens.push(value.to_string());
            }
        }
    }

    tokens
}

fn resolve_at_file_spec(
    spec: &str,
    cwd: &Path,
    source_path: &Path,
    remote_cwd: &str,
) -> Result<LabAtFileSpec> {
    let Some(raw_path) = spec.strip_prefix('@') else {
        return Err(Error::validation_invalid_argument(
            "at_file",
            "Lab offload can only materialize @file arguments",
            Some(spec.to_string()),
            None,
        ));
    };
    if raw_path.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "at_file",
            "Lab offload cannot materialize empty @file argument '@'",
            Some(spec.to_string()),
            Some(vec![
                "Pass @path/to/file for file-backed command arguments.".to_string(),
            ]),
        ));
    }
    if raw_path.contains("://") {
        return Err(Error::validation_invalid_argument(
            "at_file",
            "Lab offload only supports local filesystem @file arguments",
            Some(spec.to_string()),
            Some(vec![
                "Generate or download the file locally before offloading to Lab.".to_string(),
                "Pass inline command input if the target command supports it.".to_string(),
            ]),
        ));
    }

    let expanded = PathBuf::from(shellexpand::tilde(raw_path).to_string());
    let mut candidates = Vec::new();
    if expanded.is_absolute() || raw_path.starts_with("~/") {
        candidates.push(expanded.clone());
    } else {
        candidates.push(cwd.join(&expanded));
        let source_relative = source_path.join(&expanded);
        if !candidates
            .iter()
            .any(|candidate| candidate == &source_relative)
        {
            candidates.push(source_relative);
        }
    }

    let mut tried = Vec::new();
    for candidate in candidates {
        tried.push(candidate.display().to_string());
        if !candidate.is_file() {
            continue;
        }
        let local_path = candidate.canonicalize().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("canonicalize Lab @file {}", candidate.display())),
            )
        })?;
        let remote_path = remote_path_for_at_file(&local_path, raw_path, remote_cwd);
        return Ok(LabAtFileSpec {
            original_spec: spec.to_string(),
            local_path,
            remote_spec: format!("@{remote_path}"),
            remote_path,
        });
    }

    Err(Error::validation_invalid_argument(
        "at_file",
        "Lab offload cannot materialize @file argument because the controller-side file does not exist",
        Some(spec.to_string()),
        Some(tried),
    ))
}

fn remote_path_for_at_file(local_path: &Path, raw_path: &str, remote_cwd: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(local_path.display().to_string().as_bytes());
    digest.update(b"\0");
    digest.update(raw_path.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    let filename = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("input");
    format!(
        "{}/.homeboy/lab-at-files/{}-{}",
        remote_cwd.trim_end_matches('/'),
        &digest[..16],
        sanitize_remote_filename(filename)
    )
}

fn sanitize_remote_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}
