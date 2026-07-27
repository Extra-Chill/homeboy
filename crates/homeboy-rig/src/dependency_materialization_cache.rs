//! Content-addressed dependency materialization cache shared by local and Lab rig runs.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::expand::expand_vars_with_settings;
use crate::runner::{dependency_step_cwd, resolve_dependency_output_path};
use crate::spec::{
    DependencyMaterializationOutputKind, DependencyMaterializationStepSpec, RigSpec,
};
use homeboy_core::error::{Error, Result};

const SCHEMA: &str = "homeboy/dependency-materialization-cache/v1";
const MAX_TOOL_VERSION_BYTES: usize = 4096;
const TOOL_VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    schema: String,
    key: String,
    provenance: Provenance,
    outputs: Vec<Output>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Provenance {
    rig_id: String,
    step_id: String,
    executor: String,
    source: String,
    platform: String,
    environment_sha256: String,
    tools: Vec<ToolIdentity>,
    inputs: Vec<Input>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ToolIdentity {
    command: String,
    executable: String,
    version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    path: String,
    status: String,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Output {
    path: String,
    kind: String,
    sha256: String,
    bytes: u64,
}

pub enum CacheResult {
    Hit { bytes: u64 },
    Miss { reason: String },
    Saved { bytes: u64 },
}

pub struct DependencyMaterializationCache {
    root: PathBuf,
    entry: PathBuf,
    key: String,
    workspace: PathBuf,
    outputs: Vec<(String, PathBuf, DependencyMaterializationOutputKind)>,
    provenance: Provenance,
}

impl DependencyMaterializationCache {
    pub fn new(
        rig: &RigSpec,
        step: &DependencyMaterializationStepSpec,
        settings: &[(String, String)],
    ) -> Result<Option<Self>> {
        if step.cache_key_inputs.is_empty() || step.expected_outputs.is_empty() {
            return Ok(None);
        }
        let workspace = dependency_step_cwd(rig, step).map(PathBuf::from).unwrap_or(
            std::env::current_dir().map_err(|error| io_error("read current directory", error))?,
        );
        let mut inputs = step
            .cache_key_inputs
            .iter()
            .map(|input| {
                let path =
                    contained_path(&workspace, &expand_vars_with_settings(rig, input, settings))?;
                let relative = path.strip_prefix(&workspace).unwrap().display().to_string();
                Ok(Input {
                    path: relative,
                    status: if path.is_file() {
                        "present".to_string()
                    } else {
                        "missing".to_string()
                    },
                    sha256: path.is_file().then(|| hash_file(&path)).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        inputs.sort_by(|left, right| left.path.cmp(&right.path));
        inputs.dedup_by(|left, right| left.path == right.path);
        let mut outputs = Vec::new();
        for output in &step.expected_outputs {
            if !output.required {
                continue;
            }
            let path = resolve_dependency_output_path(
                rig,
                step,
                &expand_vars_with_settings(rig, &output.path, settings),
            );
            let path = contained_path(&workspace, path)?;
            let relative = path.strip_prefix(&workspace).unwrap().display().to_string();
            outputs.push((relative, path, output.kind));
        }
        outputs.sort_by(|left, right| left.0.cmp(&right.0));
        let executor = step
            .command
            .as_ref()
            .map(|command| format!("command:{command}"))
            .or_else(|| {
                step.provider
                    .as_ref()
                    .map(|provider| format!("provider:{provider}"))
            })
            .unwrap_or_default();
        let source = crate::runner::head_sha_and_branch(&workspace.display().to_string())
            .0
            .unwrap_or_else(|| hash_directory(&workspace).unwrap_or_default());
        let mut environment = BTreeMap::new();
        for (key, value) in &step.env {
            environment.insert(key.clone(), expand_vars_with_settings(rig, value, settings));
        }
        for (key, value) in settings {
            environment.insert(format!("setting:{key}"), value.clone());
        }
        // The assembled toolchain PATH is deliberately NOT hashed here. It is a
        // host-shaped string that changes whenever an unrelated version-managed
        // toolchain is installed or removed, which invalidated every cached
        // materialization on the machine. The stable identity the cache needs is
        // `provenance.tools` (resolved executable path + version), which is
        // resolved *through* that PATH. A step that declares its own `PATH` in
        // `step.env` keeps it in the key via the `step.env` loop above — the
        // previous unconditional insert clobbered that declared value.
        environment.insert(
            "homeboy".to_string(),
            homeboy_product_identity::product_version().to_string(),
        );
        let environment_sha256 =
            hash_bytes(&serde_json::to_vec(&environment).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize dependency cache environment".to_string()),
                )
            })?);
        let provenance = Provenance {
            rig_id: rig.id.clone(),
            step_id: step.id.clone(),
            executor,
            source,
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            environment_sha256,
            tools: resolved_tool_identities(step)?,
            inputs,
        };
        let key = hash_bytes(
            &serde_json::to_vec(&(
                SCHEMA,
                &provenance,
                outputs
                    .iter()
                    .map(|(path, _, kind)| (path, format!("{kind:?}")))
                    .collect::<Vec<_>>(),
            ))
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize dependency cache key".to_string()),
                )
            })?,
        );
        let root = cache_root()?;
        Ok(Some(Self {
            entry: root.join(&key),
            root,
            key,
            workspace,
            outputs,
            provenance,
        }))
    }

    pub fn restore(&self) -> Result<CacheResult> {
        let _lock = self.lock()?;
        let manifest_path = self.entry.join("manifest.json");
        let manifest: Manifest = match read_json::<Manifest>(&manifest_path) {
            Ok(manifest)
                if manifest.schema == SCHEMA
                    && manifest.key == self.key
                    && manifest.provenance == self.provenance =>
            {
                manifest
            }
            Ok(_) => {
                return self.evidence(CacheResult::Miss {
                    reason: "manifest_mismatch".to_string(),
                })
            }
            Err(_) => {
                return self.evidence(CacheResult::Miss {
                    reason: "entry_missing_or_invalid".to_string(),
                })
            }
        };
        if !self.manifest_outputs_match(&manifest.outputs) {
            return self.evidence(CacheResult::Miss {
                reason: "manifest_outputs_mismatch".to_string(),
            });
        }
        for output in &manifest.outputs {
            let source = self.entry.join("outputs").join(&output.path);
            if !output_matches(&source, &output.kind, &output.sha256) {
                return self.evidence(CacheResult::Miss {
                    reason: "corrupt_entry".to_string(),
                });
            }
        }
        let staging = self.workspace.join(format!(
            ".homeboy-cache-restore-{}-{}",
            self.key,
            std::process::id()
        ));
        clear_path(&staging)?;
        fs::create_dir_all(&staging)
            .map_err(|error| io_error("create dependency cache restore staging", error))?;
        let mut bytes = 0;
        for output in &manifest.outputs {
            let (_, destination, kind) = self
                .outputs
                .iter()
                .find(|(path, _, _)| path == &output.path)
                .ok_or_else(|| {
                    Error::internal_unexpected(
                        "dependency cache manifest output not declared".to_string(),
                    )
                })?;
            let source = self.entry.join("outputs").join(&output.path);
            let _destination = contained_path(&self.workspace, destination)?;
            let staged = staging.join(&output.path);
            copy_replace(&source, &staged)?;
            if !kind_matches(&staged, *kind) || hash_path(&staged)? != output.sha256 {
                let _ = clear_path(&staging);
                return self.evidence(CacheResult::Miss {
                    reason: "restore_verification_failed".to_string(),
                });
            }
            bytes += output.bytes;
        }
        publish_restore_transaction(&self.workspace, &staging, &self.outputs)?;
        self.evidence(CacheResult::Hit { bytes })
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn save(&self) -> Result<()> {
        let _lock = self.lock()?;
        let staging = self
            .root
            .join(format!(".{}-{}.tmp", self.key, std::process::id()));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(staging.join("outputs"))
            .map_err(|error| io_error("create dependency cache staging", error))?;
        let mut outputs = Vec::new();
        for (relative, source, kind) in &self.outputs {
            let source = contained_path(&self.workspace, source)?;
            if !kind_matches(&source, *kind) {
                return Err(Error::validation_invalid_argument(
                    "dependency_materialization",
                    "declared cache output is missing or has the wrong kind",
                    Some(source.display().to_string()),
                    None,
                ));
            }
            let destination = staging.join("outputs").join(relative);
            copy_replace(&source, &destination)?;
            outputs.push(Output {
                path: relative.clone(),
                kind: kind_name(*kind).to_string(),
                sha256: hash_path(&source)?,
                bytes: path_bytes(&source)?,
            });
        }
        let manifest = Manifest {
            schema: SCHEMA.to_string(),
            key: self.key.clone(),
            provenance: self.provenance.clone(),
            outputs,
        };
        write_json(&staging.join("manifest.json"), &manifest)?;
        if self.entry.exists() {
            fs::remove_dir_all(&self.entry)
                .map_err(|error| io_error("replace dependency cache entry", error))?;
        }
        fs::rename(&staging, &self.entry)
            .map_err(|error| io_error("publish dependency cache entry", error))?;
        let bytes = manifest.outputs.iter().map(|output| output.bytes).sum();
        self.evidence(CacheResult::Saved { bytes })?;
        Ok(())
    }

    fn manifest_outputs_match(&self, manifest_outputs: &[Output]) -> bool {
        let mut expected = self
            .outputs
            .iter()
            .map(|(path, _, kind)| (path.as_str(), kind_name(*kind)))
            .collect::<Vec<_>>();
        let mut actual = manifest_outputs
            .iter()
            .map(|output| (output.path.as_str(), output.kind.as_str()))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        actual.sort_unstable();
        expected == actual
    }

    fn lock(&self) -> Result<File> {
        fs::create_dir_all(&self.root)
            .map_err(|error| io_error("create dependency cache root", error))?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root.join(format!(".{}.lock", self.key)))
            .map_err(|error| io_error("open dependency cache lock", error))?;
        file.lock_exclusive()
            .map_err(|error| io_error("lock dependency cache entry", error))?;
        Ok(file)
    }

    fn evidence(&self, result: CacheResult) -> Result<CacheResult> {
        let (status, reason, bytes) = match &result {
            CacheResult::Hit { bytes } => ("hit", None, *bytes),
            CacheResult::Miss { reason } => ("miss", Some(reason.as_str()), 0),
            CacheResult::Saved { bytes } => ("saved", None, *bytes),
        };
        let evidence = serde_json::json!({ "schema": SCHEMA, "key": self.key, "status": status, "reason": reason, "bytes": bytes, "provenance": self.provenance, "cache_root": self.root });
        let event_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path = self.root.join(format!(
            "evidence-{}-{}-{}.json",
            self.key,
            std::process::id(),
            event_id
        ));
        if fs::write(&path, serde_json::to_vec(&evidence).unwrap_or_default()).is_ok() {
            let _ = crate::local_artifact::register_current_run_artifact(
                "dependency_materialization_cache",
                &path,
            );
        }
        Ok(result)
    }
}

/// Durable root shared by local and runner-side Homeboy processes. Lab invokes
/// the same rig materialization contract on the runner outside its workspace.
pub fn cache_root() -> Result<PathBuf> {
    Ok(homeboy_paths::homeboy_data()?
        .join("cache")
        .join("dependency-materialization")
        .join("v1"))
}

fn resolved_tool_identities(step: &DependencyMaterializationStepSpec) -> Result<Vec<ToolIdentity>> {
    let Some(command) = step.command.as_deref() else {
        return Ok(step
            .provider
            .as_ref()
            .map(|provider| {
                vec![ToolIdentity {
                    command: format!("provider:{provider}"),
                    executable: format!("provider:{provider}"),
                    version: "provider-contract-v1".to_string(),
                }]
            })
            .unwrap_or_default());
    };
    let command = command.split_whitespace().next().unwrap_or_default();
    if command.is_empty() {
        return Ok(Vec::new());
    }
    let resolved = crate::toolchain::command_step_path()
        .as_deref()
        .and_then(|path| {
            std::env::split_paths(path)
                .map(|directory| directory.join(command))
                .find(|path| path.is_file())
        });
    let executable = resolved
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| command.to_string());
    let version = resolved
        .as_ref()
        .and_then(|path| bounded_tool_version(path))
        .filter(|version| !version.is_empty())
        .unwrap_or_else(|| "unavailable".to_string());
    Ok(vec![ToolIdentity {
        command: command.to_string(),
        executable,
        version,
    }])
}

fn bounded_tool_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
    let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = stdout_tx.send(read_limited(&mut stdout));
    });
    std::thread::spawn(move || {
        let _ = stderr_tx.send(read_limited(&mut stderr));
    });
    let deadline = Instant::now() + TOOL_VERSION_TIMEOUT;
    loop {
        if child.try_wait().ok()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Descendants can inherit a pipe after the probed process exits. Preserve
    // the process deadline for both readers rather than extending it per pipe.
    let remaining = deadline.saturating_duration_since(Instant::now());
    let stdout = stdout_rx.recv_timeout(remaining).ok()?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let stderr = stderr_rx.recv_timeout(remaining).ok()?;
    let bytes = if stdout.is_empty() { stderr } else { stdout };
    String::from_utf8_lossy(&bytes).trim().to_string().into()
}

fn read_limited(reader: &mut impl Read) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAX_TOOL_VERSION_BYTES);
    let mut chunk = [0; 1024];
    while bytes.len() < MAX_TOOL_VERSION_BYTES {
        let limit = (MAX_TOOL_VERSION_BYTES - bytes.len()).min(chunk.len());
        match reader.read(&mut chunk[..limit]) {
            Ok(0) | Err(_) => break,
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
        }
    }
    bytes
}

fn publish_restore_transaction(
    workspace: &Path,
    staging: &Path,
    outputs: &[(String, PathBuf, DependencyMaterializationOutputKind)],
) -> Result<()> {
    let backup = workspace.join(format!(".homeboy-cache-backup-{}", std::process::id()));
    clear_path(&backup)?;
    fs::create_dir_all(&backup)
        .map_err(|error| io_error("create dependency cache rollback staging", error))?;
    let mut backed_up = Vec::new();
    let mut published = Vec::new();
    let result = (|| {
        for (relative, destination, _) in outputs {
            let destination = contained_path(workspace, destination)?;
            if destination.exists() {
                let prior = backup.join(relative);
                if let Some(parent) = prior.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        io_error("create dependency cache rollback parent", error)
                    })?;
                }
                fs::rename(&destination, prior)
                    .map_err(|error| io_error("stage dependency cache rollback", error))?;
                backed_up.push(destination);
            }
        }
        for (relative, destination, _) in outputs {
            let destination = contained_path(workspace, destination)?;
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| io_error("create dependency cache output parent", error))?;
            }
            fs::rename(staging.join(relative), &destination)
                .map_err(|error| io_error("publish dependency cache restore", error))?;
            published.push(destination);
            if should_fail_restore_publish() {
                return Err(Error::internal_unexpected(
                    "injected dependency cache restore publish failure".to_string(),
                ));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for destination in published {
            let _ = clear_path(&destination);
        }
        for destination in backed_up {
            let relative = destination.strip_prefix(workspace).map_err(|error| {
                Error::internal_unexpected(format!(
                    "derive dependency cache rollback path: {error}"
                ))
            })?;
            let prior = backup.join(relative);
            if prior.exists() {
                let _ = fs::rename(prior, destination);
            }
        }
    }
    let _ = clear_path(&backup);
    let _ = clear_path(staging);
    result
}

#[cfg(test)]
static RESTORE_PUBLISH_FAILURE_AFTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

#[cfg(test)]
pub fn inject_restore_publish_failure_after(publishes: usize) {
    RESTORE_PUBLISH_FAILURE_AFTER.store(publishes, std::sync::atomic::Ordering::SeqCst);
}

fn should_fail_restore_publish() -> bool {
    #[cfg(test)]
    {
        let remaining = RESTORE_PUBLISH_FAILURE_AFTER.load(std::sync::atomic::Ordering::SeqCst);
        if remaining == 0 {
            return true;
        }
        RESTORE_PUBLISH_FAILURE_AFTER.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    false
}

fn clear_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error("inspect dependency cache staging", error)),
    }
    .map_err(|error| io_error("clear dependency cache staging", error))
}

fn contained_path(root: &Path, path: impl AsRef<Path>) -> Result<PathBuf> {
    let path =
        homeboy_paths::resolve_contained_local_path(root, path, "dependency_materialization")
            .map_err(Into::into)?;
    let root =
        fs::canonicalize(root).map_err(|error| io_error("resolve dependency workspace", error))?;
    let mut ancestor = path.as_path();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(resolved) => {
                if resolved.starts_with(&root) {
                    return Ok(path);
                }
                return Err(Error::validation_invalid_argument(
                    "dependency_materialization",
                    "cache path resolves outside the dependency workspace",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor.parent().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "dependency_materialization",
                        "cache path has no existing ancestor in the dependency workspace",
                        Some(path.display().to_string()),
                        None,
                    )
                })?;
            }
            Err(error) => return Err(io_error("resolve dependency cache path", error)),
        }
    }
}
fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T> {
    serde_json::from_slice(
        &fs::read(path).map_err(|error| io_error("read dependency cache manifest", error))?,
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "dependency_materialization",
            format!("invalid dependency cache manifest: {error}"),
            Some(path.display().to_string()),
            None,
        )
    })
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize dependency cache manifest".to_string()),
            )
        })?,
    )
    .map_err(|error| io_error("write dependency cache manifest", error))
}
fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).map_err(|error| io_error("read dependency cache file", error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error("hash dependency cache file", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn hash_path(path: &Path) -> Result<String> {
    if path.is_file() {
        return hash_file(path);
    }
    hash_directory(path)
}
fn hash_directory(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(
            file.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        );
        hasher.update(hash_file(&file)?.as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn collect_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .map_err(|error| io_error("read dependency cache directory", error))?
    {
        let path = entry
            .map_err(|error| io_error("read dependency cache entry", error))?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("inspect dependency cache entry", error))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::validation_invalid_argument(
                "dependency_materialization",
                "cache outputs must not contain symbolic links",
                Some(path.display().to_string()),
                None,
            ));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            files.push(path);
        }
    }
    Ok(())
}
fn path_bytes(path: &Path) -> Result<u64> {
    if path.is_file() {
        return Ok(fs::metadata(path)
            .map_err(|error| io_error("inspect dependency cache file", error))?
            .len());
    }
    let mut files = Vec::new();
    collect_files(path, path, &mut files)?;
    files.into_iter().try_fold(0, |total, file| {
        Ok(total
            + fs::metadata(file)
                .map_err(|error| io_error("inspect dependency cache file", error))?
                .len())
    })
}
fn kind_name(kind: DependencyMaterializationOutputKind) -> &'static str {
    match kind {
        DependencyMaterializationOutputKind::File => "file",
        DependencyMaterializationOutputKind::Dir => "dir",
        DependencyMaterializationOutputKind::Path => "path",
    }
}
fn kind_matches(path: &Path, kind: DependencyMaterializationOutputKind) -> bool {
    match kind {
        DependencyMaterializationOutputKind::File => path.is_file(),
        DependencyMaterializationOutputKind::Dir => path.is_dir(),
        DependencyMaterializationOutputKind::Path => path.exists(),
    }
}
fn output_matches(path: &Path, kind: &str, sha256: &str) -> bool {
    let kind = match kind {
        "file" => DependencyMaterializationOutputKind::File,
        "dir" => DependencyMaterializationOutputKind::Dir,
        "path" => DependencyMaterializationOutputKind::Path,
        _ => return false,
    };
    kind_matches(path, kind) && hash_path(path).is_ok_and(|hash| hash == sha256)
}
fn copy_replace(source: &Path, destination: &Path) -> Result<()> {
    if fs::symlink_metadata(source)
        .map_err(|error| io_error("inspect dependency cache output", error))?
        .file_type()
        .is_symlink()
    {
        return Err(Error::validation_invalid_argument(
            "dependency_materialization",
            "cache outputs must not contain symbolic links",
            Some(source.display().to_string()),
            None,
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create dependency cache output parent", error))?;
    }
    let staging = destination.with_extension(format!("homeboy-cache-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .or_else(|_| fs::remove_file(&staging))
            .map_err(|error| io_error("clear dependency cache restore staging", error))?;
    }
    if source.is_file() {
        fs::copy(source, &staging)
            .map_err(|error| io_error("stage dependency cache file", error))?;
    } else {
        copy_directory(source, &staging)?;
    }
    if destination.exists() {
        if destination.is_dir() {
            fs::remove_dir_all(destination)
        } else {
            fs::remove_file(destination)
        }
        .map_err(|error| io_error("replace dependency cache output", error))?;
    }
    fs::rename(staging, destination)
        .map_err(|error| io_error("publish dependency cache output", error))?;
    Ok(())
}
fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .map_err(|error| io_error("create dependency cache directory", error))?;
    for entry in
        fs::read_dir(source).map_err(|error| io_error("read dependency cache directory", error))?
    {
        let entry = entry.map_err(|error| io_error("read dependency cache entry", error))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| io_error("inspect dependency cache entry", error))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::validation_invalid_argument(
                "dependency_materialization",
                "cache outputs must not contain symbolic links",
                Some(entry.path().display().to_string()),
                None,
            ));
        }
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), target)
                .map_err(|error| io_error("copy dependency cache file", error))?;
        } else {
            return Err(Error::validation_invalid_argument(
                "dependency_materialization",
                "cache outputs must be regular files or directories",
                Some(entry.path().display().to_string()),
                None,
            ));
        }
    }
    Ok(())
}
fn io_error(context: &str, error: std::io::Error) -> Error {
    Error::internal_io(error.to_string(), Some(context.to_string()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tool(script: &str) -> tempfile::TempPath {
        let file = tempfile::NamedTempFile::new().expect("tool");
        std::fs::write(file.path(), script).expect("tool script");
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))
            .expect("tool mode");
        file.into_temp_path()
    }

    #[test]
    fn tool_version_probe_limits_captured_output() {
        let tool = tool("#!/bin/sh\nyes x | head -c 8192\n");

        let version = bounded_tool_version(&tool).expect("version");

        assert_eq!(version.len(), MAX_TOOL_VERSION_BYTES - 1);
    }

    #[test]
    fn tool_version_probe_times_out() {
        let tool = tool("#!/bin/sh\nsleep 30\n");
        let started = Instant::now();

        let _ = bounded_tool_version(&tool);

        assert!(
            started.elapsed() < TOOL_VERSION_TIMEOUT + Duration::from_secs(2),
            "version probe must terminate within its timeout"
        );
    }
}
