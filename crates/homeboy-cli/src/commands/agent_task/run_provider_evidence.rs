//! Provider-evidence admission, storage, and prompt adapters.

use super::run::*;

pub(crate) fn validate_provider_evidence_inputs(
    inputs: &[AgentTaskProviderEvidenceInput],
    prompt: Option<&str>,
) -> homeboy::core::Result<()> {
    admit_provider_evidence_inputs(inputs)?;
    // A workspace-derived allowlist is not available during argument parsing.
    // Cook validates declared paths after rewriting them to their projections.
    if inputs.is_empty() {
        validate_provider_evidence_prompt(prompt, &std::collections::BTreeSet::new())?;
    }
    Ok(())
}

pub(crate) fn admit_provider_evidence_inputs(
    inputs: &[AgentTaskProviderEvidenceInput],
) -> homeboy::core::Result<Vec<AdmittedProviderEvidenceSource>> {
    let mut ids = std::collections::BTreeSet::new();
    let mut sources = std::collections::BTreeSet::new();
    let mut canonical_sources = std::collections::BTreeSet::new();
    let mut admitted_sources = Vec::with_capacity(inputs.len());
    for input in inputs {
        if input.id.trim().is_empty()
            || input.id.contains(['/', '\\'])
            || input.id == "."
            || input.id == ".."
        {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence ids must be non-empty path-free names",
                Some(input.id.clone()),
                None,
            ));
        }
        if !ids.insert(input.id.clone()) || !sources.insert(input.source.clone()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence ids and sources must be unique",
                Some(input.source.clone()),
                None,
            ));
        }
        let source = admit_provider_evidence_source(&input.source)?;
        if source.kind == ProviderEvidenceKind::File
            && (!input.include.is_empty() || !input.exclude.is_empty())
        {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence include and exclude apply only to directories",
                Some(input.id.clone()),
                None,
            ));
        }
        if !canonical_sources.insert(source.canonical_path.clone()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "provider-evidence",
                "provider evidence sources must be unique after canonical path resolution",
                Some(format!(
                    "supplied_path={} canonical_path={}",
                    source.supplied_path.display(),
                    source.canonical_path.display()
                )),
                None,
            ));
        }
        admitted_sources.push(source);
    }
    Ok(admitted_sources)
}

pub(super) fn validate_provider_evidence_prompt(
    prompt: Option<&str>,
    projected_paths: &std::collections::BTreeSet<String>,
) -> homeboy::core::Result<()> {
    let Some(prompt) = prompt else { return Ok(()) };
    let undeclared = classified_absolute_host_paths_in_provider_prompt(prompt)?
        .into_iter()
        .filter(|path| !is_projected_provider_evidence_path(&path.path, projected_paths))
        .collect::<Vec<_>>();
    if !undeclared.is_empty() {
        let paths = undeclared
            .iter()
            .map(|path| format!("`{}` ({})", path.path, path.classification))
            .collect::<Vec<_>>()
            .join(", ");
        let classification_evidence = undeclared
            .iter()
            .map(ClassifiedHostPath::diagnostic_evidence)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(homeboy::core::Error::validation_invalid_argument(
            "prompt",
            format!("prompt names undeclared absolute evidence paths: {paths}"),
            Some(classification_evidence),
            Some(vec!["Declare each path with --provider-evidence '{\"id\":\"evidence\",\"source\":\"/absolute/path\"}' so Homeboy projects it into the provider workspace.".to_string()]),
        ));
    }
    Ok(())
}

pub(super) const MAX_PROVIDER_PROMPT_PATH_SCAN_BYTES: usize = 256 * 1024;

pub(super) const MAX_PROVIDER_PROMPT_PATH_DIAGNOSTIC_TOKEN_BYTES: usize = 160;

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ClassifiedHostPath {
    pub(super) path: String,
    pub(super) token: String,
    pub(super) classification: &'static str,
}

impl ClassifiedHostPath {
    fn diagnostic_evidence(&self) -> String {
        format!(
            "path={} classification={} token={}",
            self.path, self.classification, self.token
        )
    }
}

/// Extract concrete Unix absolute paths from the bounded provider prompt
/// surface. URL references, quoted examples, code, and slash-separated
/// concepts are excluded; host paths need an explicit file syntax, an
/// explicit assignment, or a recognized Unix root.
#[cfg(test)]
pub(super) fn absolute_host_paths_in_provider_prompt(
    prompt: &str,
) -> homeboy::core::Result<Vec<String>> {
    Ok(classified_absolute_host_paths_in_provider_prompt(prompt)?
        .into_iter()
        .map(|path| path.path)
        .collect())
}

pub(super) fn classified_absolute_host_paths_in_provider_prompt(
    prompt: &str,
) -> homeboy::core::Result<Vec<ClassifiedHostPath>> {
    if prompt.len() > MAX_PROVIDER_PROMPT_PATH_SCAN_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "prompt",
            "provider prompt exceeds the absolute-path scan limit",
            Some(format!("{} bytes", prompt.len())),
            None,
        ));
    }

    let mut paths = std::collections::BTreeSet::new();
    let mut fenced = false;
    for line in prompt.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || line.starts_with('\t') || line.starts_with("    ") {
            continue;
        }
        for token in line.split_whitespace() {
            if token.contains("://") && !token.contains("file://") {
                continue;
            }
            let mut candidate = token;
            let file_url = token.find("file://");
            if let Some(file) = file_url {
                if slash_is_quoted(token, file) {
                    continue;
                }
                let rest = &token[file + "file://".len()..];
                candidate = if rest.starts_with('/') {
                    rest
                } else {
                    rest.find('/').map(|offset| &rest[offset..]).unwrap_or("")
                };
            }
            let mut offset = 0;
            while offset < candidate.len() {
                let Some(relative) = candidate[offset..].find('/') else {
                    break;
                };
                let start = offset + relative;
                if slash_is_quoted(candidate, start) {
                    offset = start + 1;
                    continue;
                }
                if start != 0
                    && !matches!(
                        candidate[..start].chars().next_back(),
                        Some('=' | ':' | '(' | '[' | '{' | '<')
                    )
                {
                    offset = start + 1;
                    continue;
                }
                let path = &candidate[start..];
                let end = path.find(|character: char| {
                    character.is_whitespace()
                        || matches!(
                            character,
                            '\'' | '"' | '`' | ')' | ']' | '}' | '>' | ',' | ';' | '!' | '?'
                        )
                });
                let path = path[..end.unwrap_or(path.len())].trim_end_matches('.');
                let prefix = (start > 0)
                    .then(|| candidate[..start].chars().next_back())
                    .flatten();
                let assignment = prefix == Some('=');
                if let Some(classification) =
                    classify_absolute_host_path(path, file_url.is_some(), assignment)
                {
                    paths.insert(ClassifiedHostPath {
                        path: path.to_string(),
                        token: bounded_prompt_path_token(token),
                        classification,
                    });
                }
                offset = start.saturating_add(path.len()).max(start + 1);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

pub(super) fn slash_is_quoted(token: &str, offset: usize) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in token[..offset].chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
        } else if matches!(character, '\'' | '"' | '`') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        }
    }
    quote.is_some()
}

pub(super) fn classify_absolute_host_path(
    path: &str,
    file_url: bool,
    assignment: bool,
) -> Option<&'static str> {
    let trimmed = path.trim_start_matches('/');
    let root = trimmed.split('/').next()?;
    if trimmed.is_empty() || path.contains('#') || is_device_pseudo_file(trimmed) {
        return None;
    }
    if file_url {
        return Some("file-uri");
    }
    if assignment {
        return Some("explicit-path-assignment");
    }
    if matches!(
        root,
        "bin"
            | "boot"
            | "dev"
            | "etc"
            | "home"
            | "lib"
            | "lib64"
            | "media"
            | "mnt"
            | "opt"
            | "private"
            | "proc"
            | "root"
            | "run"
            | "sbin"
            | "srv"
            | "sys"
            | "tmp"
            | "usr"
            | "var"
            | "Users"
            | "Volumes"
            | "workspace"
    ) {
        return Some("unix-host-root");
    }
    None
}

/// Device pseudo-files such as `/dev/null` appear in diff headers and shell
/// redirections; they are never projectable host evidence.
fn is_device_pseudo_file(trimmed: &str) -> bool {
    matches!(
        trimmed,
        "dev/null"
            | "dev/stdin"
            | "dev/stdout"
            | "dev/stderr"
            | "dev/zero"
            | "dev/random"
            | "dev/urandom"
            | "dev/tty"
            | "dev/fd"
    ) || trimmed.starts_with("dev/fd/")
}

pub(super) fn bounded_prompt_path_token(token: &str) -> String {
    if token.len() <= MAX_PROVIDER_PROMPT_PATH_DIAGNOSTIC_TOKEN_BYTES {
        return token.to_string();
    }
    let end = token
        .char_indices()
        .take_while(|(index, _)| *index < MAX_PROVIDER_PROMPT_PATH_DIAGNOSTIC_TOKEN_BYTES)
        .last()
        .map_or(0, |(index, character)| index + character.len_utf8());
    format!("{}...", &token[..end])
}

pub(super) fn is_projected_provider_evidence_path(
    path: &str,
    projected_paths: &std::collections::BTreeSet<String>,
) -> bool {
    projected_paths.contains(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderEvidenceKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub(crate) struct AdmittedProviderEvidenceSource {
    supplied_path: PathBuf,
    canonical_path: PathBuf,
    approved_root: Option<PathBuf>,
    kind: ProviderEvidenceKind,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl AdmittedProviderEvidenceSource {
    pub(super) fn copy_path(&self) -> &Path {
        // Only the macOS system alias is safe to bypass during the no-follow walk.
        self.approved_root
            .as_ref()
            .map_or(&self.supplied_path, |_| &self.canonical_path)
    }
}

pub(super) fn admit_provider_evidence_source(
    source: &str,
) -> homeboy::core::Result<AdmittedProviderEvidenceSource> {
    let supplied_path = PathBuf::from(source);
    let metadata = std::fs::symlink_metadata(&supplied_path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence sources must be existing absolute regular files or directories",
            Some(format!("{source}: {error}")),
            None,
        )
    })?;
    let kind = if metadata.is_file() {
        ProviderEvidenceKind::File
    } else if metadata.is_dir() {
        ProviderEvidenceKind::Directory
    } else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence sources must be existing absolute regular files or directories without symlinks",
            Some(source.to_string()),
            None,
        ));
    };
    if !supplied_path.is_absolute() || metadata.file_type().is_symlink() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence sources must be existing absolute regular files or directories without symlinks",
            Some(source.to_string()),
            None,
        ));
    }
    let canonical_path = supplied_path.canonicalize().map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source could not be canonicalized",
            Some(format!("{source}: {error}")),
            None,
        )
    })?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(AdmittedProviderEvidenceSource {
        approved_root: cfg!(target_os = "macos")
            .then(|| approved_macos_temporary_root(&supplied_path, &canonical_path))
            .flatten(),
        supplied_path,
        canonical_path,
        kind,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

// Pure mapping: only these fixed system aliases may bypass the no-follow walk.
pub(super) fn macos_system_alias(canonical_path: &Path) -> Option<(&'static Path, PathBuf)> {
    const ALIASES: &[(&str, &str)] = &[
        ("/tmp", "/private/tmp"),
        ("/var", "/private/var"),
        ("/etc", "/private/etc"),
    ];
    let canonical_path = canonical_path.to_str()?;
    ALIASES.iter().find_map(|(alias, root)| {
        canonical_path
            .strip_prefix(&format!("{root}/"))
            .map(|suffix| (Path::new(root), Path::new(alias).join(suffix)))
    })
}

pub(super) fn approved_macos_temporary_root(
    supplied_path: &Path,
    canonical_path: &Path,
) -> Option<PathBuf> {
    let (root, alias) = macos_system_alias(canonical_path)?;
    // Compare spellings, not normalized Path components (which discard `.`).
    (supplied_path.as_os_str() == canonical_path.as_os_str()
        || supplied_path.as_os_str() == alias.as_os_str())
    .then(|| root.to_path_buf())
}

pub(super) fn approved_provider_evidence_spellings(canonical_path: &Path) -> Vec<PathBuf> {
    let mut spellings = vec![canonical_path.to_path_buf()];
    if cfg!(target_os = "macos") {
        if let Some((_, alias)) = macos_system_alias(canonical_path) {
            spellings.push(alias);
        }
    }
    spellings
}

pub(super) fn verify_admitted_provider_evidence_source(
    source: &AdmittedProviderEvidenceSource,
) -> homeboy::core::Result<()> {
    let metadata = std::fs::symlink_metadata(source.copy_path()).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(format!("{}: {error}", source.copy_path().display())),
            None,
        )
    })?;
    let canonical = source.copy_path().canonicalize().map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(format!("{}: {error}", source.copy_path().display())),
            None,
        )
    })?;
    let kind_matches = match source.kind {
        ProviderEvidenceKind::File => metadata.is_file(),
        ProviderEvidenceKind::Directory => metadata.is_dir(),
    };
    #[cfg(unix)]
    let identity_matches = {
        use std::os::unix::fs::MetadataExt;
        metadata.dev() == source.device && metadata.ino() == source.inode
    };
    #[cfg(not(unix))]
    let identity_matches = true;
    if !kind_matches
        || !identity_matches
        || metadata.file_type().is_symlink()
        || canonical != source.canonical_path
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(source.copy_path().display().to_string()),
            None,
        ));
    }
    Ok(())
}

pub(super) fn provider_evidence_controller_provenance_from_admitted(
    inputs: &[AgentTaskProviderEvidenceInput],
    admitted: &[AdmittedProviderEvidenceSource],
) -> Vec<Value> {
    inputs
        .iter()
        .zip(admitted)
        .map(|(input, source)| {
            serde_json::json!({
                "id": input.id,
                "supplied_path": "[redacted]",
                "canonical_path": "[redacted]",
                "canonical_path_sha256": format!(
                    "sha256:{}",
                    homeboy_engine_primitives::content_hash::sha256_hex(
                        source.canonical_path.as_os_str().as_encoded_bytes()
                    )
                ),
                "approved_root": source.approved_root.as_ref().map(|_| "[redacted]"),
            })
        })
        .collect()
}

pub(super) const PROVIDER_EVIDENCE_STORE: &str = "provider-evidence";

pub(super) fn provider_evidence_store() -> homeboy::core::Result<PathBuf> {
    homeboy::core::paths::homeboy_data_store(PROVIDER_EVIDENCE_STORE)
}

pub(super) fn provider_evidence_blob_path(store: &Path, digest: &str) -> PathBuf {
    store
        // A file has its own readable directory. Native tools authorize the
        // parent directory before reading a file; a shared blob-store parent
        // would expose unrelated evidence through glob/grep.
        .join("files")
        .join(digest.trim_start_matches("sha256:"))
        .join("input")
}

pub(super) fn provider_evidence_tree_path(store: &Path, digest: &str) -> PathBuf {
    store
        .join("trees")
        .join(digest.trim_start_matches("sha256:"))
}

pub(super) fn directory_evidence_plan(
    input: &AgentTaskProviderEvidenceInput,
    source: &AdmittedProviderEvidenceSource,
) -> homeboy::core::Result<super::provider_evidence::DirectoryEvidencePlan> {
    verify_admitted_provider_evidence_source(source)?;
    let plan = super::provider_evidence::plan_directory_evidence(
        source.copy_path(),
        &input.include,
        &input.exclude,
        &super::provider_evidence::DirectoryEvidenceLimits::production(MAX_PROVIDER_EVIDENCE_BYTES),
    )?;
    verify_admitted_provider_evidence_source(source)?;
    Ok(plan)
}

pub(crate) fn projected_provider_evidence(
    inputs: &[AgentTaskProviderEvidenceInput],
    workspace: Option<&str>,
) -> homeboy::core::Result<Vec<Value>> {
    let _workspace = workspace.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence requires a bound Cook workspace",
            None,
            None,
        )
    })?;
    let store = provider_evidence_store()?;
    let admitted = admit_provider_evidence_inputs(inputs)?;
    projected_provider_evidence_from_admitted(inputs, &admitted, &store)
}

pub(super) fn projected_provider_evidence_from_admitted(
    inputs: &[AgentTaskProviderEvidenceInput],
    admitted: &[AdmittedProviderEvidenceSource],
    store: &Path,
) -> homeboy::core::Result<Vec<Value>> {
    let mut projected = Vec::with_capacity(inputs.len());
    for (input, source) in inputs.iter().zip(admitted) {
        verify_admitted_provider_evidence_source(source)?;
        if source.kind == ProviderEvidenceKind::Directory {
            let plan = directory_evidence_plan(input, source)?;
            projected.push(super::provider_evidence::directory_projection_value(
                &input.id,
                Path::new(&input.source)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_ref(),
                &provider_evidence_tree_path(store, &plan.digest),
                &plan,
                false,
            ));
            continue;
        }
        let digest = homeboy_engine_primitives::content_hash::sha256_file(source.copy_path())?;
        let size = std::fs::metadata(source.copy_path())
            .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))?
            .len();
        projected.push(serde_json::json!({
            "id": input.id,
            "path": provider_evidence_blob_path(store, &digest),
            "read_only": true,
            "sha256": format!("sha256:{digest}"),
            "size_bytes": size,
            "transport": "content-addressed-blob/v1",
            "artifact": {"digest": format!("sha256:{digest}"), "size_bytes": size},
        }));
    }
    Ok(projected)
}

pub(crate) fn projected_provider_evidence_paths(
    evidence: &[Value],
) -> std::collections::BTreeSet<String> {
    evidence
        .iter()
        .flat_map(|input| super::provider_evidence::projected_directory_member_paths(input))
        .collect()
}

#[cfg(test)]
pub(super) fn project_provider_evidence_inputs(
    inputs: &[AgentTaskProviderEvidenceInput],
    workspace: &Path,
    prompt: Option<&str>,
) -> homeboy::core::Result<Vec<Value>> {
    let admitted = admit_provider_evidence_inputs(inputs)?;
    let store = workspace
        .parent()
        .unwrap_or(workspace)
        .join("provider-evidence");
    let projected = project_admitted_provider_evidence_inputs_at(inputs, &admitted, &store)?;
    validate_provider_evidence_prompt(prompt, &projected_provider_evidence_paths(&projected))?;
    Ok(projected)
}

pub(crate) fn project_admitted_provider_evidence_inputs(
    inputs: &[AgentTaskProviderEvidenceInput],
    admitted: &[AdmittedProviderEvidenceSource],
) -> homeboy::core::Result<Vec<Value>> {
    project_admitted_provider_evidence_inputs_at(inputs, admitted, &provider_evidence_store()?)
}

pub(super) fn project_admitted_provider_evidence_inputs_at(
    inputs: &[AgentTaskProviderEvidenceInput],
    admitted: &[AdmittedProviderEvidenceSource],
    store: &Path,
) -> homeboy::core::Result<Vec<Value>> {
    let mut projected = Vec::with_capacity(inputs.len());
    for (input, source) in inputs.iter().zip(admitted) {
        verify_admitted_provider_evidence_source(source)?;
        if source.kind == ProviderEvidenceKind::Directory {
            let plan = directory_evidence_plan(input, source)?;
            let destination = provider_evidence_tree_path(store, &plan.digest);
            super::provider_evidence::copy_directory_evidence(
                source.copy_path(),
                &plan,
                &destination,
            )?;
            projected.push(super::provider_evidence::directory_projection_value(
                &input.id,
                Path::new(&input.source)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_ref(),
                &destination,
                &plan,
                true,
            ));
            continue;
        }
        let staging = store
            .join("staging")
            .join(format!("evidence-{}", uuid::Uuid::new_v4()));
        let (bytes, digest) = match secure_provider_evidence_copy(source, &staging) {
            Ok(copied) => copied,
            Err(error) => {
                let _ = std::fs::remove_file(&staging);
                return Err(error);
            }
        };
        let destination = provider_evidence_blob_path(store, &digest);
        if let Err(error) = publish_provider_evidence_blob(&staging, &destination, bytes, &digest) {
            let _ = std::fs::remove_file(&staging);
            return Err(error);
        }
        let mut projection = serde_json::json!({
            "id": input.id,
            "path": destination,
            "read_only": true,
        });
        projection["size_bytes"] = serde_json::json!(bytes);
        projection["sha256"] = serde_json::json!(&digest);
        projection["transport"] = serde_json::json!("content-addressed-blob/v1");
        projection["artifact"] = serde_json::json!({
            "digest": digest,
            "size_bytes": bytes,
        });
        // Do not persist a controller-local source path in the provider plan.
        projection["provenance"] = serde_json::json!({
            "kind": "controller-file",
            "source_name": Path::new(&input.source).file_name().unwrap_or_default().to_string_lossy(),
        });
        projection["visibility"] = serde_json::json!("private");
        projection["redaction"] = serde_json::json!("withhold-content");
        projection["ownership"] = serde_json::json!({
            "owner": "controller-artifact-store",
            "scope": "content-addressed",
        });
        projected.push(projection);
    }
    Ok(projected)
}

pub(super) fn publish_provider_evidence_blob(
    staging: &Path,
    destination: &Path,
    size_bytes: u64,
    digest: &str,
) -> homeboy::core::Result<()> {
    let publish = (|| {
        let parent = destination.parent().expect("evidence blob parent");
        for parent in [parent.parent().expect("evidence file store"), parent] {
            match std::fs::create_dir(parent) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(parent).map_err(|error| {
                        homeboy::core::Error::internal_io(
                            error.to_string(),
                            Some(parent.display().to_string()),
                        )
                    })?;
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err(homeboy::core::Error::validation_invalid_argument(
                            "provider-evidence",
                            "content-addressed provider evidence storage cannot traverse symlinks",
                            Some(parent.display().to_string()),
                            None,
                        ));
                    }
                }
                Err(error) => {
                    return Err(homeboy::core::Error::internal_io(
                        error.to_string(),
                        Some(parent.display().to_string()),
                    ));
                }
            }
        }
        match std::fs::hard_link(staging, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(destination).map_err(|error| {
                    homeboy::core::Error::internal_io(
                        error.to_string(),
                        Some(destination.display().to_string()),
                    )
                })?;
                let actual_digest =
                    homeboy_engine_primitives::content_hash::sha256_file(destination)?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() != size_bytes
                    || format!("sha256:{actual_digest}") != digest
                {
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "provider-evidence",
                        "content-addressed provider evidence storage is corrupt",
                        Some(destination.display().to_string()),
                        None,
                    ));
                }
            }
            Err(error) => {
                return Err(homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some(destination.display().to_string()),
                ));
            }
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_file(staging).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(staging.display().to_string()))
    });
    publish.and(cleanup)
}

#[cfg(unix)]
pub(super) fn secure_provider_evidence_copy(
    source: &AdmittedProviderEvidenceSource,
    destination: &Path,
) -> homeboy::core::Result<(u64, String)> {
    secure_provider_evidence_copy_with_limit(source, destination, MAX_PROVIDER_EVIDENCE_BYTES)
}

#[cfg(unix)]
pub(super) fn secure_provider_evidence_copy_with_limit(
    source: &AdmittedProviderEvidenceSource,
    destination: &Path,
    max_bytes: u64,
) -> homeboy::core::Result<(u64, String)> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    fn directory(path: &Path, create: bool) -> homeboy::core::Result<std::fs::File> {
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
        for component in path.components().skip(1) {
            let name = std::ffi::CString::new(component.as_os_str().as_bytes()).map_err(|_| {
                homeboy::core::Error::validation_invalid_argument(
                    "provider-evidence",
                    "evidence path contains NUL",
                    None,
                    None,
                )
            })?;
            if create {
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
            let fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
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
    let source_path = source.copy_path();
    let source_parent = directory(source_path.parent().expect("source parent"), false)?;
    let source_name =
        std::ffi::CString::new(source_path.file_name().expect("source name").as_bytes())
            .expect("source name");
    let source_fd = unsafe {
        libc::openat(
            source_parent.as_raw_fd(),
            source_name.as_ptr(),
            // A replacement FIFO must not block admission before we can reject
            // its descriptor type and identity below.
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if source_fd < 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source could not be securely opened",
            Some(source_path.display().to_string()),
            None,
        ));
    }
    let input = unsafe { std::fs::File::from_raw_fd(source_fd) };
    let metadata = input.metadata().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(source_path.display().to_string()),
        )
    })?;
    use std::os::unix::fs::MetadataExt;
    if !metadata.is_file() || metadata.dev() != source.device || metadata.ino() != source.inode {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(source_path.display().to_string()),
            None,
        ));
    }
    if metadata.len() > max_bytes {
        return Err(provider_evidence_size_error(
            source_path,
            metadata.len(),
            max_bytes,
        ));
    }
    let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(homeboy::core::Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            Some(source_path.display().to_string()),
        ));
    }
    let parent = directory(destination.parent().expect("destination parent"), true)?;
    let final_name = std::ffi::CString::new(
        destination
            .file_name()
            .expect("destination name")
            .as_bytes(),
    )
    .expect("destination name");
    let temporary_name = std::ffi::CString::new(format!(".evidence-{}", uuid::Uuid::new_v4()))
        .expect("temporary name");
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
    let mut input = input;
    let mut digest = homeboy_engine_primitives::content_hash::sha256_hasher();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(source_path.display().to_string()),
            )
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max_bytes {
            drop(output);
            unsafe { libc::unlinkat(parent.as_raw_fd(), temporary_name.as_ptr(), 0) };
            return Err(provider_evidence_size_error(source_path, total, max_bytes));
        }
        output.write_all(&buffer[..read]).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        })?;
        sha2::Digest::update(&mut digest, &buffer[..read]);
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
    Ok((
        total,
        format!("sha256:{:x}", sha2::Digest::finalize(digest)),
    ))
}

#[cfg(not(unix))]
pub(super) fn secure_provider_evidence_copy(
    source: &AdmittedProviderEvidenceSource,
    destination: &Path,
) -> homeboy::core::Result<(u64, String)> {
    secure_provider_evidence_copy_with_limit(source, destination, MAX_PROVIDER_EVIDENCE_BYTES)
}

#[cfg(not(unix))]
pub(super) fn secure_provider_evidence_copy_with_limit(
    source: &AdmittedProviderEvidenceSource,
    destination: &Path,
    max_bytes: u64,
) -> homeboy::core::Result<(u64, String)> {
    let source_path = source.copy_path();
    let canonical = source_path.canonicalize().map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(format!("{}: {error}", source_path.display())),
            None,
        )
    })?;
    if canonical != source.canonical_path {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(source_path.display().to_string()),
            None,
        ));
    }
    let bytes = std::fs::read(source_path).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(source_path.display().to_string()),
        )
    })?;
    let current_canonical = source_path.canonicalize().ok();
    if current_canonical.as_ref() != Some(&source.canonical_path) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "provider-evidence",
            "provider evidence source identity changed after validation",
            Some(source_path.display().to_string()),
            None,
        ));
    }
    if bytes.len() as u64 > max_bytes {
        return Err(provider_evidence_size_error(
            source_path,
            bytes.len() as u64,
            max_bytes,
        ));
    }
    std::fs::create_dir_all(destination.parent().expect("destination parent")).map_err(
        |error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(destination.display().to_string()),
            )
        },
    )?;
    std::fs::write(destination, &bytes).map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some(destination.display().to_string()),
        )
    })?;
    Ok((
        bytes.len() as u64,
        format!(
            "sha256:{}",
            homeboy_engine_primitives::content_hash::sha256_hex(&bytes)
        ),
    ))
}

pub(super) fn provider_evidence_size_error(
    source: &Path,
    actual_bytes: u64,
    max_bytes: u64,
) -> homeboy::core::Error {
    let mut error = homeboy::core::Error::validation_invalid_argument(
        "provider-evidence",
        format!(
            "provider evidence is {actual_bytes} bytes; the content-addressed artifact limit is {max_bytes} bytes"
        ),
        Some(source.display().to_string()),
        Some(vec![format!(
            "Provide a fixture at or below {max_bytes} bytes, then rerun the same `homeboy agent-task cook --provider-evidence ...` command."
        )]),
    );
    error.details["limit_bytes"] = serde_json::json!(max_bytes);
    error.details["actual_bytes"] = serde_json::json!(actual_bytes);
    error.details["transport"] = serde_json::json!("content-addressed-blob/v1");
    error
}

pub(crate) fn rewrite_provider_evidence_prompt(
    prompt: &mut Option<String>,
    inputs: &[AgentTaskProviderEvidenceInput],
    admitted: &[AdmittedProviderEvidenceSource],
    projected: &[Value],
    projected_paths: &std::collections::BTreeSet<String>,
) -> homeboy::core::Result<()> {
    let Some(prompt) = prompt else { return Ok(()) };
    let mut rewritten = prompt.clone();
    for ((input, source), projection) in inputs.iter().zip(admitted).zip(projected) {
        verify_admitted_provider_evidence_source(source)?;
        let destination = projection["path"].as_str().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(format!(
                "provider evidence projection '{}' has no path",
                input.id
            ))
        })?;
        for spelling in approved_provider_evidence_spellings(&source.canonical_path) {
            rewritten = rewritten.replace(&spelling.display().to_string(), destination);
        }
    }
    *prompt = rewritten;
    validate_provider_evidence_prompt(Some(prompt.as_str()), projected_paths)
}
