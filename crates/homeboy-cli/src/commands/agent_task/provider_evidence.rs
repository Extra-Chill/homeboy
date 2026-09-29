use sha2::Digest;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use homeboy_engine_primitives::content_hash::{
    directory_evidence_entry_record, directory_evidence_tree_digest,
    is_safe_evidence_relative_path, PROVIDER_EVIDENCE_DIRECTORY_TRANSPORT,
};
use serde_json::{json, Value};

pub(crate) const MAX_DIRECTORY_EVIDENCE_SCAN_ENTRIES: usize = 8_192;
pub(crate) const MAX_DIRECTORY_EVIDENCE_DEPTH: usize = 24;
pub(crate) const DEFAULT_EVIDENCE_MEDIA_FILE_CAP_BYTES: u64 = 1024 * 1024;
const OMITTED_EVIDENCE_SAMPLE: usize = 8;

const DEFAULT_MEDIA_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "avif", "heic", "mp4", "mov",
    "webm", "avi", "mkv", "m4v",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryEvidenceLimits {
    pub max_bytes: u64,
    pub max_entries: usize,
    pub max_depth: usize,
    pub media_file_cap_bytes: u64,
}

impl DirectoryEvidenceLimits {
    pub(crate) fn production(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            max_entries: MAX_DIRECTORY_EVIDENCE_SCAN_ENTRIES,
            max_depth: MAX_DIRECTORY_EVIDENCE_DEPTH,
            media_file_cap_bytes: DEFAULT_EVIDENCE_MEDIA_FILE_CAP_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryEvidenceFile {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OmittedEvidenceFile {
    relative_path: String,
    size_bytes: u64,
    reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryEvidencePlan {
    pub entries: Vec<DirectoryEvidenceFile>,
    pub total_bytes: u64,
    pub digest: String,
    omitted: Vec<OmittedEvidenceFile>,
}

pub(crate) fn plan_directory_evidence(
    root: &Path,
    include: &[String],
    exclude: &[String],
    limits: &DirectoryEvidenceLimits,
) -> homeboy::core::Result<DirectoryEvidencePlan> {
    validate_evidence_globs(include, "include")?;
    validate_evidence_globs(exclude, "exclude")?;
    let mut selected = Vec::new();
    let mut omitted = Vec::new();
    let mut scanned = 0usize;
    walk_evidence_directory(
        root,
        root,
        "",
        0,
        include,
        exclude,
        limits,
        &mut scanned,
        &mut selected,
        &mut omitted,
    )?;
    if selected.is_empty() {
        let omitted_bytes = omitted
            .iter()
            .fold(0u64, |total, file| total.saturating_add(file.size_bytes));
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence directory projection selected no files",
            Some(format!(
                "omitted_files={} omitted_bytes={omitted_bytes}",
                omitted.len()
            )),
            Some(vec![
                "Add a relative include such as \"website/**\" or remove an exclude that dropped every file."
                    .to_string(),
                format!(
                    "Media files over {} bytes are omitted unless an include matches them.",
                    limits.media_file_cap_bytes
                ),
            ]),
        ));
    }
    selected.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let total_bytes = selected
        .iter()
        .fold(0u64, |total, file| total.saturating_add(file.size_bytes));
    if total_bytes > limits.max_bytes {
        return Err(directory_budget_error(
            root,
            &selected,
            &omitted,
            total_bytes,
            limits,
        ));
    }
    for file in &mut selected {
        let (size, digest) = hash_evidence_file(root, &file.relative_path)?;
        if size != file.size_bytes {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence file changed while the directory was projected",
                Some(file.relative_path.clone()),
                None,
            ));
        }
        file.sha256 = format!("sha256:{digest}");
    }
    let records = selected
        .iter()
        .map(|file| {
            directory_evidence_entry_record(&file.relative_path, &file.sha256, file.size_bytes)
        })
        .collect::<Vec<_>>();
    omitted.sort_by(|left, right| {
        right
            .size_bytes
            .cmp(&left.size_bytes)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    Ok(DirectoryEvidencePlan {
        digest: directory_evidence_tree_digest(&records),
        entries: selected,
        total_bytes,
        omitted,
    })
}

pub(crate) fn directory_projection_value(
    id: &str,
    source_name: &str,
    path: &Path,
    plan: &DirectoryEvidencePlan,
    materialized: bool,
) -> Value {
    let entries = plan
        .entries
        .iter()
        .map(|file| {
            json!({
                "path": file.relative_path,
                "sha256": file.sha256,
                "size_bytes": file.size_bytes,
            })
        })
        .collect::<Vec<_>>();
    let sample = plan
        .omitted
        .iter()
        .take(OMITTED_EVIDENCE_SAMPLE)
        .map(|file| {
            json!({
                "path": file.relative_path,
                "size_bytes": file.size_bytes,
                "reason": file.reason,
            })
        })
        .collect::<Vec<_>>();
    let mut projection = json!({
        "id": id,
        "path": path,
        "read_only": true,
        "size_bytes": plan.total_bytes,
        "sha256": plan.digest,
        "transport": PROVIDER_EVIDENCE_DIRECTORY_TRANSPORT,
        "entries": entries,
        "omitted": {
            "count": plan.omitted.len(),
            "bytes": plan
                .omitted
                .iter()
                .fold(0u64, |total, file| total.saturating_add(file.size_bytes)),
            "sample": sample,
        },
        "artifact": {
            "digest": plan.digest,
            "size_bytes": plan.total_bytes,
            "kind": "directory",
        },
    });
    if materialized {
        projection["provenance"] = json!({
            "kind": "controller-directory",
            "source_name": source_name,
            "selected_files": plan.entries.len(),
        });
        projection["visibility"] = json!("private");
        projection["redaction"] = json!("withhold-content");
        projection["ownership"] = json!({
            "owner": "controller-artifact-store",
            "scope": "content-addressed",
        });
    }
    projection
}

pub(crate) fn projected_directory_member_paths(projection: &Value) -> Vec<String> {
    let Some(root) = projection["path"].as_str() else {
        return Vec::new();
    };
    let mut paths = vec![root.to_string()];
    if let Some(entries) = projection["entries"].as_array() {
        for entry in entries {
            if let Some(relative) = entry["path"].as_str() {
                paths.push(format!("{root}/{relative}"));
            }
        }
    }
    paths
}

pub(crate) fn copy_directory_evidence(
    root: &Path,
    plan: &DirectoryEvidencePlan,
    destination: &Path,
) -> homeboy::core::Result<()> {
    if destination_matches(destination, plan)? {
        return Ok(());
    }
    let staging = destination
        .parent()
        .and_then(|parent| parent.parent())
        .unwrap_or(destination)
        .join("staging")
        .join(format!("evidence-{}", uuid::Uuid::new_v4()));
    let copy = (|| {
        create_evidence_directory(&staging)?;
        for file in &plan.entries {
            write_evidence_file(root, &staging, file)?;
        }
        publish_evidence_tree(&staging, destination, plan)?;
        freeze_projected_directories(destination)
    })();
    if copy.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    copy
}

fn destination_matches(
    destination: &Path,
    plan: &DirectoryEvidencePlan,
) -> homeboy::core::Result<bool> {
    let metadata = match std::fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            ));
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() || !metadata.permissions().readonly()
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "content-addressed provider evidence storage is corrupt",
            Some(destination.display().to_string()),
            None,
        ));
    }
    let mut actual_paths = std::collections::BTreeSet::new();
    let mut scanned = 0usize;
    collect_projected_tree_paths(destination, destination, &mut actual_paths, &mut scanned)?;
    let expected_paths = plan
        .entries
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if actual_paths != expected_paths {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "content-addressed provider evidence storage is corrupt",
            Some(destination.display().to_string()),
            None,
        ));
    }
    for file in &plan.entries {
        let path = join_evidence_relative(destination, &file.relative_path)?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "content-addressed provider evidence storage is corrupt",
                Some(format!("{}: {error}", path.display())),
                None,
            )
        })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != file.size_bytes
            || !metadata.permissions().readonly()
        {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "content-addressed provider evidence storage is corrupt",
                Some(path.display().to_string()),
                None,
            ));
        }
        let actual = hash_evidence_file(destination, &file.relative_path)?;
        if format!("sha256:{}", actual.1) != file.sha256 || actual.0 != file.size_bytes {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "content-addressed provider evidence storage is corrupt",
                Some(path.display().to_string()),
                None,
            ));
        }
    }
    Ok(true)
}

fn collect_projected_tree_paths(
    root: &Path,
    directory: &Path,
    paths: &mut std::collections::BTreeSet<String>,
    scanned: &mut usize,
) -> homeboy::core::Result<()> {
    let metadata = std::fs::symlink_metadata(directory).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(directory.display().to_string()))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || !metadata.permissions().readonly()
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "content-addressed provider evidence storage is corrupt",
            Some(directory.display().to_string()),
            None,
        ));
    }
    let children = std::fs::read_dir(directory).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(directory.display().to_string()))
    })?;
    for entry in children {
        *scanned += 1;
        if *scanned > MAX_DIRECTORY_EVIDENCE_SCAN_ENTRIES {
            return Err(scan_limit_error(
                root,
                "content-addressed provider evidence storage exceeds its entry limit",
                MAX_DIRECTORY_EVIDENCE_SCAN_ENTRIES,
            ));
        }
        let entry = entry.map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(directory.display().to_string()),
            )
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "content-addressed provider evidence storage is corrupt",
                Some(path.display().to_string()),
                None,
            ));
        }
        if metadata.is_dir() {
            collect_projected_tree_paths(root, &path, paths, scanned)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| {
                    homeboy::core::Error::internal_unexpected(
                        "projected evidence path escaped its tree",
                    )
                })?
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            if !paths.insert(relative) {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "content-addressed provider evidence storage is corrupt",
                    Some(path.display().to_string()),
                    None,
                ));
            }
        }
    }
    Ok(())
}

fn freeze_projected_directories(root: &Path) -> homeboy::core::Result<()> {
    let mut directories = vec![root.to_path_buf()];
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(directory.display().to_string()),
            )
        })? {
            let entry = entry.map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(directory.display().to_string()),
                )
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(path.display().to_string()),
                )
            })?;
            if metadata.file_type().is_symlink() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "projected provider evidence cannot contain symlinks",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            if metadata.is_dir() {
                directories.push(path.clone());
                pending.push(path);
            }
        }
    }
    for directory in directories.into_iter().rev() {
        let mut permissions = std::fs::metadata(&directory)
            .map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(directory.display().to_string()),
                )
            })?
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() & !0o222);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(true);
        std::fs::set_permissions(&directory, permissions).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(directory.display().to_string()),
            )
        })?;
    }
    Ok(())
}

fn publish_evidence_tree(
    staging: &Path,
    destination: &Path,
    plan: &DirectoryEvidencePlan,
) -> homeboy::core::Result<()> {
    if let Some(parent) = destination.parent() {
        create_evidence_directory(parent)?;
    }
    match std::fs::rename(staging, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_dir_all(staging);
            if destination_matches(destination, plan)? {
                Ok(())
            } else {
                Err(homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "content-addressed provider evidence storage is corrupt",
                    Some(destination.display().to_string()),
                    None,
                ))
            }
        }
        Err(error) => Err(homeboy::core::Error::internal_io(
            error.to_string(),
            Some(destination.display().to_string()),
        )),
    }
}

fn walk_evidence_directory(
    root: &Path,
    directory: &Path,
    relative: &str,
    depth: usize,
    include: &[String],
    exclude: &[String],
    limits: &DirectoryEvidenceLimits,
    scanned: &mut usize,
    selected: &mut Vec<DirectoryEvidenceFile>,
    omitted: &mut Vec<OmittedEvidenceFile>,
) -> homeboy::core::Result<()> {
    if depth > limits.max_depth {
        return Err(scan_limit_error(
            root,
            "provider evidence directory exceeds the projection depth limit",
            limits.max_depth,
        ));
    }
    let iterator = std::fs::read_dir(directory).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence directory could not be read",
            Some(format!("{}: {error}", directory.display())),
            None,
        )
    })?;
    let mut children = Vec::new();
    for entry in iterator {
        *scanned += 1;
        if *scanned > limits.max_entries {
            return Err(scan_limit_error(
                root,
                "provider evidence directory scan exceeded its entry limit",
                limits.max_entries,
            ));
        }
        children.push(entry.map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(directory.display().to_string()),
            )
        })?);
    }
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence path is not valid UTF-8",
                Some(directory.display().to_string()),
                None,
            ));
        };
        if name.contains('\0') || name == "." || name == ".." {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence path is not safe to project",
                Some(name.to_string()),
                None,
            ));
        }
        let child_relative = if relative.is_empty() {
            name.to_string()
        } else {
            format!("{relative}/{name}")
        };
        if !is_safe_evidence_relative_path(&child_relative) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence path is not safe to project",
                Some(child_relative),
                None,
            ));
        }
        let file_type = entry.file_type().map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(child_relative.clone()))
        })?;
        if file_type.is_symlink() {
            classify_symlink(
                &child_relative,
                include,
                exclude,
                file_type.is_dir(),
                omitted,
            )?;
            continue;
        }
        if file_type.is_dir() {
            if exclude_matches(&child_relative, exclude)
                || !include_may_match_under(&child_relative, include)
            {
                omitted.push(OmittedEvidenceFile {
                    relative_path: child_relative.clone(),
                    size_bytes: 0,
                    reason: if exclude_matches(&child_relative, exclude) {
                        "exclude"
                    } else {
                        "include"
                    },
                });
                continue;
            }
            walk_evidence_directory(
                root,
                &entry.path(),
                &child_relative,
                depth + 1,
                include,
                exclude,
                limits,
                scanned,
                selected,
                omitted,
            )?;
            continue;
        }
        if !file_type.is_file() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence directories can only project regular files",
                Some(child_relative),
                None,
            ));
        }
        let metadata = entry.metadata().map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(child_relative.clone()))
        })?;
        let size_bytes = metadata.len();
        if let Some(reason) = omission_reason(
            &child_relative,
            size_bytes,
            include,
            exclude,
            limits.media_file_cap_bytes,
        ) {
            omitted.push(OmittedEvidenceFile {
                relative_path: child_relative,
                size_bytes,
                reason,
            });
            continue;
        }
        selected.push(DirectoryEvidenceFile {
            relative_path: child_relative,
            sha256: String::new(),
            size_bytes,
        });
    }
    Ok(())
}

fn classify_symlink(
    relative: &str,
    include: &[String],
    exclude: &[String],
    is_dir: bool,
    omitted: &mut Vec<OmittedEvidenceFile>,
) -> homeboy::core::Result<()> {
    if exclude_matches(relative, exclude)
        || (!is_dir && !explicitly_included(relative, include) && is_media_path(relative))
    {
        omitted.push(OmittedEvidenceFile {
            relative_path: relative.to_string(),
            size_bytes: 0,
            reason: "symlink",
        });
        return Ok(());
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "provider-evidence",
        "provider evidence directories cannot follow symlinks",
        Some(relative.to_string()),
        Some(vec![
            "Remove the symlink or exclude its relative path before projecting the directory."
                .to_string(),
        ]),
    ))
}

fn omission_reason(
    relative: &str,
    size_bytes: u64,
    include: &[String],
    exclude: &[String],
    media_file_cap_bytes: u64,
) -> Option<&'static str> {
    if exclude_matches(relative, exclude) {
        return Some("exclude");
    }
    let included = explicitly_included(relative, include);
    if !include.is_empty() && !included {
        return Some("include");
    }
    if !included && is_media_path(relative) && size_bytes > media_file_cap_bytes {
        return Some("default-media-cap");
    }
    None
}

fn include_may_match_under(directory: &str, include: &[String]) -> bool {
    if include.is_empty() {
        return true;
    }
    include.iter().any(|pattern| {
        let pattern = pattern.trim().trim_matches('/');
        if pattern.is_empty() {
            return false;
        }
        if !pattern.contains('/') || pattern.starts_with("**/") || pattern.starts_with("**") {
            return true;
        }
        let prefix = pattern.split('/').next().unwrap_or(pattern);
        if prefix == "*" || prefix == "**" {
            return true;
        }
        directory == prefix
            || directory.starts_with(&format!("{prefix}/"))
            || prefix.starts_with(&format!("{directory}/"))
            || pattern.starts_with(&format!("{directory}/"))
    })
}

fn explicitly_included(relative: &str, include: &[String]) -> bool {
    include
        .iter()
        .any(|pattern| evidence_glob_matches(pattern, relative))
}

fn exclude_matches(relative: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|pattern| {
        evidence_glob_matches(pattern, relative) || directory_exclude_matches(pattern, relative)
    })
}

fn directory_exclude_matches(pattern: &str, directory: &str) -> bool {
    let Some(prefix) = pattern.strip_suffix("/**") else {
        return false;
    };
    directory == prefix.trim_matches('/')
        || directory.starts_with(&format!("{}/", prefix.trim_matches('/')))
}

fn is_media_path(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .is_some_and(|extension| DEFAULT_MEDIA_EXTENSIONS.contains(&extension.as_str()))
}

fn validate_evidence_globs(patterns: &[String], field: &str) -> homeboy::core::Result<()> {
    for pattern in patterns {
        let raw = pattern.trim();
        let trimmed = raw.trim_matches('/');
        if trimmed.is_empty()
            || raw.starts_with('/')
            || raw.starts_with('\\')
            || raw.contains('\\')
            || raw.chars().any(char::is_control)
            || (raw.len() >= 3
                && raw.as_bytes()[0].is_ascii_alphabetic()
                && raw.as_bytes()[1] == b':'
                && matches!(raw.as_bytes()[2], b'/' | b'\\'))
            || trimmed
                .split('/')
                .any(|component| component == "." || component == "..")
        {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                format!("provider evidence {field} patterns must be relative and path-safe"),
                Some(pattern.clone()),
                None,
            ));
        }
        for component in trimmed.split('/') {
            if component == "**" {
                continue;
            }
            if glob::Pattern::new(component).is_err() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    format!("provider evidence {field} pattern is not a valid glob"),
                    Some(pattern.clone()),
                    None,
                ));
            }
        }
    }
    Ok(())
}

fn evidence_glob_matches(pattern: &str, relative: &str) -> bool {
    let pattern = pattern.trim().trim_matches('/');
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('/') && !pattern.contains("**") {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        return glob_segment_matches(pattern, name);
    }
    glob_path_matches(pattern, relative)
}

fn glob_path_matches(pattern: &str, relative: &str) -> bool {
    let pattern = pattern
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let relative = relative
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    match_segments(&pattern, &relative)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    let mut pattern_index = 0;
    let mut path_index = 0;
    while pattern_index < pattern.len() {
        if pattern[pattern_index] == "**" {
            if pattern_index + 1 == pattern.len() {
                return true;
            }
            for skip in 0..=path.len().saturating_sub(path_index) {
                if match_segments(&pattern[pattern_index + 1..], &path[path_index + skip..]) {
                    return true;
                }
            }
            return false;
        }
        if path_index >= path.len()
            || !glob_segment_matches(pattern[pattern_index], path[path_index])
        {
            return false;
        }
        pattern_index += 1;
        path_index += 1;
    }
    path_index == path.len()
}

fn glob_segment_matches(pattern: &str, segment: &str) -> bool {
    glob::Pattern::new(pattern).is_ok_and(|pattern| pattern.matches(segment))
}

fn directory_budget_error(
    root: &Path,
    selected: &[DirectoryEvidenceFile],
    omitted: &[OmittedEvidenceFile],
    actual_bytes: u64,
    limits: &DirectoryEvidenceLimits,
) -> homeboy::core::Error {
    let mut largest = selected.to_vec();
    largest.sort_by(|left, right| right.size_bytes.cmp(&left.size_bytes));
    let largest = largest
        .into_iter()
        .take(OMITTED_EVIDENCE_SAMPLE)
        .map(|file| {
            json!({
                "path": file.relative_path,
                "size_bytes": file.size_bytes,
            })
        })
        .collect::<Vec<_>>();
    let mut error = homeboy::core::Error::validation_invalid_argument(
        "provider-evidence",
        format!(
            "provider evidence directory projection is {actual_bytes} bytes; the content-addressed artifact limit is {} bytes",
            limits.max_bytes
        ),
        Some(root.display().to_string()),
        Some(vec![format!(
            "Exclude the largest paths with \"exclude\" or narrow \"include\". Media files over {} bytes are already omitted unless explicitly included.",
            limits.media_file_cap_bytes
        )]),
    );
    error.details["limit_bytes"] = json!(limits.max_bytes);
    error.details["actual_bytes"] = json!(actual_bytes);
    error.details["transport"] = json!(PROVIDER_EVIDENCE_DIRECTORY_TRANSPORT);
    error.details["largest_files"] = json!(largest);
    error.details["omitted_bytes"] = json!(omitted
        .iter()
        .fold(0u64, |total, file| total.saturating_add(file.size_bytes)));
    error
}

fn scan_limit_error(root: &Path, message: &str, limit: usize) -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "provider-evidence",
        message,
        Some(format!("{} limit={limit}", root.display())),
        Some(vec![
            "Narrow the directory with a relative include such as \"website/**\" before retrying."
                .to_string(),
        ]),
    )
}

fn join_evidence_relative(root: &Path, relative: &str) -> homeboy::core::Result<PathBuf> {
    if !is_safe_evidence_relative_path(relative) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence path is not safe to project",
            Some(relative.to_string()),
            None,
        ));
    }
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        path.push(component);
    }
    Ok(path)
}

fn hash_evidence_file(root: &Path, relative: &str) -> homeboy::core::Result<(u64, String)> {
    let path = join_evidence_relative(root, relative)?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence file disappeared during projection",
            Some(format!("{}: {error}", path.display())),
            None,
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence directories cannot follow symlinks",
            Some(relative.to_string()),
            None,
        ));
    }
    #[cfg(unix)]
    {
        hash_nofollow(&path)
    }
    #[cfg(not(unix))]
    {
        let bytes = std::fs::read(&path).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        Ok((
            bytes.len() as u64,
            homeboy_engine_primitives::content_hash::sha256_hex(&bytes),
        ))
    }
}

#[cfg(unix)]
fn hash_nofollow(path: &Path) -> homeboy::core::Result<(u64, String)> {
    let file = open_nofollow(path, false)?;
    let metadata = file.metadata().map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    if !metadata.is_file() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence directories cannot follow symlinks",
            Some(path.display().to_string()),
            None,
        ));
    }
    let mut file = file;
    let mut hasher = sha2::Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        sha2::Digest::update(&mut hasher, &buffer[..read]);
    }
    Ok((total, format!("{:x}", sha2::Digest::finalize(hasher))))
}

#[cfg(unix)]
fn macos_private_var_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(suffix) = path.to_str().and_then(|path| path.strip_prefix("/var/")) {
            if std::fs::read_link("/var").ok().as_deref() == Some(Path::new("private/var")) {
                return PathBuf::from(format!("/private/var/{suffix}"));
            }
        }
    }
    path.to_path_buf()
}

#[cfg(unix)]
fn open_nofollow(path: &Path, create_directories: bool) -> homeboy::core::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let path = macos_private_var_path(path);
    if !path.is_absolute() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "evidence paths must be absolute",
            Some(path.display().to_string()),
            None,
        ));
    }
    let root = std::ffi::CString::new("/").expect("root");
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(homeboy::core::Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            None,
        ));
    }
    let mut current = unsafe { std::fs::File::from_raw_fd(fd) };
    let components = path.components().skip(1).collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let name = std::ffi::CString::new(component.as_os_str().as_bytes()).map_err(|_| {
            homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "evidence path contains NUL",
                None,
                None,
            )
        })?;
        let last = index + 1 == components.len();
        if create_directories {
            let result = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
            if result != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(homeboy::core::Error::internal_io(
                    std::io::Error::last_os_error().to_string(),
                    Some(path.display().to_string()),
                ));
            }
        }
        let flags = if last && !create_directories {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "evidence paths cannot traverse symlink or non-directory components",
                Some(path.display().to_string()),
                None,
            ));
        }
        current = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}

fn create_evidence_directory(path: &Path) -> homeboy::core::Result<()> {
    #[cfg(unix)]
    {
        if path.exists() {
            let metadata = std::fs::symlink_metadata(path).map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(path.display().to_string()),
                )
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "evidence paths cannot traverse symlink or non-directory components",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            return Ok(());
        }
        let _ = open_nofollow(path, true)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })
    }
}

fn write_evidence_file(
    source_root: &Path,
    destination_root: &Path,
    file: &DirectoryEvidenceFile,
) -> homeboy::core::Result<()> {
    let (size, digest) = hash_evidence_file(source_root, &file.relative_path)?;
    if size != file.size_bytes || format!("sha256:{digest}") != file.sha256 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence file changed while the directory was projected",
            Some(file.relative_path.clone()),
            None,
        ));
    }
    let source = join_evidence_relative(source_root, &file.relative_path)?;
    let destination = join_evidence_relative(destination_root, &file.relative_path)?;
    if let Some(parent) = destination.parent() {
        create_evidence_directory(parent)?;
    }
    #[cfg(unix)]
    {
        write_nofollow(&source, &destination)?;
    }
    #[cfg(not(unix))]
    {
        let bytes = std::fs::read(&source).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(source.display().to_string()))
        })?;
        std::fs::write(&destination, bytes).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        })?;
        let mut permissions = std::fs::metadata(&destination)
            .map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(destination.display().to_string()),
                )
            })?
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&destination, permissions).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        })?;
    }
    let (copied_size, copied_digest) = hash_evidence_file(destination_root, &file.relative_path)?;
    if copied_size != file.size_bytes || format!("sha256:{copied_digest}") != file.sha256 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence file changed while the directory was copied",
            Some(file.relative_path.clone()),
            None,
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn write_nofollow(source: &Path, destination: &Path) -> homeboy::core::Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let mut input = open_nofollow(source, false)?;
    let parent = open_nofollow(destination.parent().expect("destination parent"), true)?;
    let final_name = std::ffi::CString::new(
        destination
            .file_name()
            .expect("destination name")
            .as_bytes(),
    )
    .expect("destination name");
    let temporary_name =
        std::ffi::CString::new(format!(".evidence-{}", uuid::Uuid::new_v4())).expect("temporary");
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o400,
        )
    };
    if fd < 0 {
        return Err(homeboy::core::Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some(destination.display().to_string()),
        ));
    }
    let mut output = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(source.display().to_string()))
        })?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        })?;
    }
    output.sync_all().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(destination.display().to_string()),
        )
    })?;
    if unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temporary_name.as_ptr(),
            parent.as_raw_fd(),
            final_name.as_ptr(),
        )
    } != 0
    {
        return Err(homeboy::core::Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some(destination.display().to_string()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_bytes: u64, max_entries: usize) -> DirectoryEvidenceLimits {
        DirectoryEvidenceLimits {
            max_bytes,
            max_entries,
            max_depth: 4,
            media_file_cap_bytes: 8,
        }
    }

    #[test]
    fn omits_huge_media_without_reading_it_and_keeps_relative_paths() {
        let temp = tempfile::tempdir().expect("capture");
        let website = temp.path().join("website");
        std::fs::create_dir(&website).expect("website");
        std::fs::write(website.join("index.html"), "<html>").expect("html");
        std::fs::create_dir(temp.path().join("receipts")).expect("receipts");
        std::fs::write(temp.path().join("receipts/note.txt"), "ok").expect("receipt");
        let screenshot = temp.path().join("screenshots");
        std::fs::create_dir(&screenshot).expect("screenshots");
        std::fs::write(screenshot.join("home.png"), vec![1u8; 32]).expect("screenshot");

        let plan = plan_directory_evidence(temp.path(), &[], &[], &limits(100, 20))
            .expect("project bounded capture");
        assert_eq!(
            plan.entries
                .iter()
                .map(|file| file.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["receipts/note.txt", "website/index.html"]
        );
        assert!(plan
            .omitted
            .iter()
            .any(|file| file.relative_path == "screenshots/home.png"
                && file.reason == "default-media-cap"));
        assert!(!plan.digest.is_empty());
    }

    #[test]
    fn budget_fallback_names_largest_files_without_copying_them() {
        let temp = tempfile::tempdir().expect("capture");
        std::fs::write(temp.path().join("large.txt"), vec![b'a'; 20]).expect("large");
        std::fs::write(temp.path().join("small.txt"), "x").expect("small");
        let error = plan_directory_evidence(temp.path(), &[], &[], &limits(4, 20))
            .expect_err("over-budget directory is rejected");
        assert_eq!(error.details["limit_bytes"], 4);
        assert_eq!(error.details["actual_bytes"], 21);
        assert_eq!(
            error.details["transport"],
            PROVIDER_EVIDENCE_DIRECTORY_TRANSPORT
        );
        assert!(error.message.contains("largest") || error.details["largest_files"].is_array());
        assert!(error.details["tried"]
            .as_array()
            .expect("remediation")
            .iter()
            .any(|item| item.as_str().unwrap_or("").contains("exclude")));
    }

    #[cfg(unix)]
    #[test]
    fn selected_symlink_is_rejected_and_not_followed() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("capture");
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "secret").expect("outside");
        let website = temp.path().join("website");
        std::fs::create_dir(&website).expect("website");
        symlink(&outside, website.join("index.html")).expect("link");
        let error = plan_directory_evidence(temp.path(), &[], &[], &limits(100, 20))
            .expect_err("selected symlink is rejected");
        assert!(error.message.contains("cannot follow symlinks"));
        assert_eq!(
            std::fs::read_to_string(&outside).expect("outside untouched"),
            "secret"
        );
    }

    #[test]
    fn include_bounds_the_scan_and_exclude_drops_matches() {
        let temp = tempfile::tempdir().expect("capture");
        std::fs::create_dir(temp.path().join("website")).expect("website");
        std::fs::write(temp.path().join("website/index.html"), "page").expect("page");
        std::fs::create_dir(temp.path().join("screenshots")).expect("screenshots");
        std::fs::write(temp.path().join("screenshots/home.png"), vec![1u8; 64]).expect("shot");
        let plan = plan_directory_evidence(
            temp.path(),
            &["website/**".to_string()],
            &[],
            &limits(100, 3),
        )
        .expect("include bounds the walk");
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].relative_path, "website/index.html");
        assert!(plan
            .omitted
            .iter()
            .any(|file| file.relative_path == "screenshots" && file.reason == "include"));
    }

    #[test]
    fn scan_limit_fails_closed_before_an_unbounded_walk() {
        let temp = tempfile::tempdir().expect("capture");
        for index in 0..4 {
            std::fs::write(temp.path().join(format!("file-{index}.txt")), "x").expect("file");
        }
        let error =
            plan_directory_evidence(temp.path(), &[], &[], &limits(100, 2)).expect_err("scan cap");
        assert!(error.message.contains("entry limit"));
    }

    #[test]
    fn rejects_absolute_and_control_character_globs() {
        let temp = tempfile::tempdir().expect("capture");
        for pattern in ["/website/**", "C:/website/**", "website/\nindex.html"] {
            let error =
                plan_directory_evidence(temp.path(), &[pattern.to_string()], &[], &limits(100, 20))
                    .expect_err("unsafe include pattern");
            assert!(
                error.message.contains("relative and path-safe"),
                "{pattern}"
            );
        }
    }
}
