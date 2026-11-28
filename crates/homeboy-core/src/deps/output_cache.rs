//! Lockfile-keyed cache of a provider's declared install outputs.
//!
//! A fresh workspace pays a full `composer install` / `npm ci` even when the
//! same lockfile was installed a minute earlier. After a successful install the
//! declared outputs (`vendor/`, `node_modules/`) are saved under a key made of
//! the provider, install argv, declared outputs, platform, tool version, and the
//! contents of the provider's key files (lockfiles and manifests). A later
//! workspace whose key matches restores that copy instead of reinstalling.
use super::provider::DependencyProviderHydrationPlan;
use super::DependencyInstallOutputKind;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const CACHE_SCHEMA: &str = "homeboy/dependency-output-cache/v1";
/// Entries kept after a publish, newest use first. Each entry is a full
/// dependency tree, so the bound is a count, not an age.
const MAX_ENTRIES: usize = 8;
const LAST_USED: &str = ".last-used";

pub(super) struct OutputCache {
    entry: PathBuf,
    root: PathBuf,
    outputs: Vec<PathBuf>,
}

impl OutputCache {
    /// The cache slot for `plan`, or `None` when the plan cannot be keyed:
    /// no key files, no outputs, or an output outside the install root.
    pub(super) fn for_plan(
        plan: &DependencyProviderHydrationPlan,
        environment: &[(String, String)],
    ) -> Option<Self> {
        if plan.cache_key_files.is_empty() || plan.outputs.is_empty() {
            return None;
        }
        let outputs = plan
            .outputs
            .iter()
            .map(|output| {
                let path = PathBuf::from(&output.path);
                path.components()
                    .all(|component| matches!(component, Component::Normal(_)))
                    .then_some(path)
            })
            .collect::<Option<Vec<_>>>()?;
        let key_files = plan
            .cache_key_files
            .iter()
            .map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                let digest = homeboy_engine_primitives::content_hash::sha256_file(path).ok()?;
                Some((name, digest))
            })
            .collect::<Option<Vec<_>>>()?;
        let identity = serde_json::to_vec(&(
            CACHE_SCHEMA,
            &plan.provider_id,
            plan.install.argv(),
            plan.outputs
                .iter()
                .map(|output| (&output.path, output_kind(output.kind)))
                .collect::<Vec<_>>(),
            std::env::consts::OS,
            std::env::consts::ARCH,
            tool_version(plan, environment)?,
            key_files,
        ))
        .ok()?;
        let root = crate::paths::homeboy_data()
            .ok()?
            .join("controller-state/dependency-output-cache");
        Some(Self {
            entry: root.join(homeboy_engine_primitives::content_hash::sha256_hex(
                &identity,
            )),
            root,
            outputs,
        })
    }

    /// Copy a saved install into `install_root`. Only a workspace that has
    /// none of the outputs yet is restored, so a partial install is never
    /// merged with a cached one.
    pub(super) fn restore(&self, install_root: &Path) -> bool {
        if !self.entry.is_dir()
            || self
                .outputs
                .iter()
                .any(|output| install_root.join(output).symlink_metadata().is_ok())
        {
            return false;
        }
        for output in &self.outputs {
            let target = install_root.join(output);
            let copied = target
                .parent()
                .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                && copy_preserving(&self.entry.join(output), &target);
            if !copied {
                for output in &self.outputs {
                    let _ = remove_path(&install_root.join(output));
                }
                return false;
            }
        }
        let _ = std::fs::write(self.entry.join(LAST_USED), b"");
        true
    }

    /// Save the outputs of a successful install. Best effort: a failure only
    /// means the next workspace installs normally.
    pub(super) fn publish(&self, install_root: &Path) {
        if self.entry.is_dir() || std::fs::create_dir_all(&self.root).is_err() {
            return;
        }
        let staging = self.root.join(format!(
            ".staging-{}-{}",
            std::process::id(),
            self.entry
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        let _ = remove_path(&staging);
        let saved = std::fs::create_dir_all(&staging).is_ok()
            && self.outputs.iter().all(|output| {
                let source = install_root.join(output);
                let target = staging.join(output);
                target
                    .parent()
                    .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                    && copy_preserving(&source, &target)
            })
            && std::fs::write(staging.join(LAST_USED), b"").is_ok()
            && std::fs::rename(&staging, &self.entry).is_ok();
        if !saved {
            let _ = remove_path(&staging);
        }
        self.evict();
    }

    fn evict(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        let mut entries = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| {
                let used = std::fs::metadata(entry.path().join(LAST_USED))
                    .and_then(|metadata| metadata.modified())
                    .ok();
                (used, entry.path())
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| right.0.cmp(&left.0));
        for (_, path) in entries.into_iter().skip(MAX_ENTRIES) {
            let _ = remove_path(&path);
        }
    }
}

fn output_kind(kind: DependencyInstallOutputKind) -> &'static str {
    match kind {
        DependencyInstallOutputKind::Path => "path",
        DependencyInstallOutputKind::File => "file",
        DependencyInstallOutputKind::Directory => "directory",
    }
}

/// The installing tool's `--version`, so a toolchain upgrade misses the cache.
/// A shell-wrapped install (`sh -c 'composer install'`) is keyed on the tool
/// it runs, not on the shell.
fn tool_version(
    plan: &DependencyProviderHydrationPlan,
    environment: &[(String, String)],
) -> Option<String> {
    let install = &plan.install;
    let tool = match install.args.as_slice() {
        [flag, script, ..] if flag == "-c" => script.split_whitespace().next()?.to_string(),
        _ => install.program.clone(),
    };
    let output = Command::new(&tool)
        .arg("--version")
        .current_dir(&install.cwd)
        .envs(environment.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Copy a tree keeping symlinks, modes, and timestamps (`node_modules/.bin`
/// and pnpm's layout are symlinks).
fn copy_preserving(source: &Path, target: &Path) -> bool {
    Command::new("cp")
        .arg("-a")
        .arg(source)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
