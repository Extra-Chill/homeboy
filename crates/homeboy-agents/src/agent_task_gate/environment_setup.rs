use super::*;

/// Resolve the real directory every gate process receives as its temporary
/// root.
///
/// Two independent constraints apply, and canonicalizing satisfies only one of
/// them:
///
/// 1. **Socket budget (#10829).** `InvocationGuard` deliberately exports a
///    short symlink alias (`<runtime-root>/<short>.t`) and validates it with
///    `enforce_path_budget`, so gate workloads can bind `AF_UNIX` sockets
///    under `$TMPDIR`. The managed directory behind that alias is ~125 bytes —
///    already past the 108-byte Linux `sun_path` capacity before a socket name
///    is appended. Resolving the alias away discards the very budget the
///    invocation layer validated.
/// 2. **Non-symlink root (#11265).** Security-sensitive child tools reject a
///    symlinked temp root outright, before writing anything into it.
///
/// A real `tmp` subdirectory created *through* the alias satisfies both: the
/// path stays short, the leaf is a genuine directory rather than a symlink,
/// and the bytes still land in managed runtime-temp storage rather than on the
/// invocation root's volume (#11125).
///
/// Isolated `HOME`/XDG directories hang off this same root, so they inherit
/// both properties instead of re-deriving them.
pub(super) fn gate_temp_root(runtime_tmpdir: &Path) -> Result<PathBuf> {
    let root = runtime_tmpdir.join("tmp");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "create isolated gate temp directory {}",
                root.display()
            )),
        )
    })?;
    Ok(root)
}

/// The gate environment for destination dependency setup. Setup runs under
/// the same resolved environment as the gates that follow it (#16024), with its
/// own invocation temp dir so `Replace` mode still has a valid TMPDIR.
pub(crate) struct GateHydrationEnvironment {
    pub(super) selected: SelectedGateEnvironment,
    pub(super) _invocation: homeboy_core::engine::invocation::InvocationGuard,
    pub(super) run_dir: homeboy_core::engine::run_dir::RunDir,
}

impl GateHydrationEnvironment {
    pub(crate) fn prepare(policy: &AgentTaskGateEnvironmentPolicy) -> Result<Self> {
        let run_dir = homeboy_core::engine::run_dir::RunDir::create()?;
        let invocation = homeboy_core::engine::invocation::InvocationGuard::acquire(
            &run_dir,
            &homeboy_core::engine::invocation::InvocationRequirements::default(),
        )?;
        let selected = selected_gate_environment(policy, Some(&invocation.context().tmp_dir))?;
        Ok(Self {
            selected,
            _invocation: invocation,
            run_dir,
        })
    }

    pub(crate) fn environment(&self) -> homeboy_core::deps::DependencyHydrationEnvironment {
        self.selected.hydration_environment()
    }

    pub(crate) fn finish(self, succeeded: bool) {
        self.run_dir.finish(succeeded);
    }
}

pub(super) fn resolve_gate_requirements(
    requirements: &AgentTaskGateRequirements,
    values: &mut BTreeMap<String, String>,
    report: &mut AgentTaskGateEnvironment,
) -> Result<()> {
    if !requirements.host_tools.is_empty() {
        let extensions = homeboy_core::extension::catalog::load_all_extensions()?;
        for tool in &requirements.host_tools {
            let mappings: Vec<_> = extensions
                .iter()
                .filter_map(|extension| extension.gate_host_tools.get(tool))
                .collect();
            if mappings.len() != 1 || mappings[0].is_empty() {
                return Err(Error::validation_invalid_argument(
                    "gate_requirements.host_tools",
                    format!("host tool '{tool}' needs exactly one extension declaration"),
                    None,
                    None,
                ));
            }
            for (name, source) in mappings[0] {
                let value = preserved_environment_value(source).map_err(|_| {
                    Error::validation_invalid_argument(
                        "gate_requirements.host_tools",
                        format!("missing host tool '{tool}' input '{name}'"),
                        None,
                        None,
                    )
                })?;
                if source.contains('/') && !Path::new(&value).exists() {
                    return Err(Error::validation_invalid_argument(
                        "gate_requirements.host_tools",
                        format!("missing host tool '{tool}' input '{name}'"),
                        None,
                        None,
                    ));
                }
                values.insert(name.clone(), value);
                report.preserved.insert(name.clone(), source.clone());
            }
        }
    }
    for name in &requirements.secrets {
        let value = std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "gate_requirements.secrets",
                    format!("missing secret '{name}'"),
                    None,
                    None,
                )
            })?;
        values.insert(name.clone(), value);
        report.declared_secret_env.push(name.clone());
    }
    Ok(())
}

pub(super) fn selected_gate_environment(
    policy: &AgentTaskGateEnvironmentPolicy,
    runtime_tmpdir: Option<&Path>,
) -> Result<SelectedGateEnvironment> {
    let gate_tmpdir = runtime_tmpdir.map(gate_temp_root).transpose()?;
    let runtime_tmpdir = gate_tmpdir.as_deref();
    let mut report = AgentTaskGateEnvironment {
        mode: policy.mode,
        ..AgentTaskGateEnvironment::default()
    };
    let mut values = policy.variables.clone();
    report.data_inputs = policy.data_inputs.clone();
    for (name, value) in &policy.variables {
        report.inherited.push(AgentTaskGateEnvironmentVariable {
            name: name.clone(),
            value: value.clone(),
        });
    }
    for (name, source) in &policy.preserve {
        let value = preserved_environment_value(source)?;
        values.insert(name.clone(), value);
        report.preserved.insert(name.clone(), source.clone());
    }
    resolve_gate_requirements(&policy.requirements, &mut values, &mut report)?;
    let host_home = std::env::var_os("HOME").map(PathBuf::from);
    let declared_playwright_cache = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").map(PathBuf::from);
    preserve_playwright_cache(
        policy,
        declared_playwright_cache.as_deref(),
        host_home.as_deref(),
        &mut values,
        &mut report,
    );

    // The invocation temp dir belongs to the shared environment definition, not
    // to individual call sites. Preflight and execution must agree on it, or
    // preflight validates a different directory than the gate will use — and,
    // under the default inherit mode, silently falls back to the ambient TMPDIR
    // or fails outright when the host has none. (#10245)
    if let Some(runtime_tmpdir) = runtime_tmpdir {
        let runtime_tmpdir = runtime_tmpdir.display().to_string();
        for name in TMPDIR_ENV_VARS {
            values.insert((*name).to_string(), runtime_tmpdir.clone());
        }
    }

    let needs_scratch = policy.isolate_home || policy.isolate_xdg;
    let scratch = if needs_scratch && runtime_tmpdir.is_none() {
        Some(tempfile::tempdir().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create isolated gate environment".to_string()),
            )
        })?)
    } else {
        None
    };
    let root = runtime_tmpdir.or_else(|| scratch.as_ref().map(|dir| dir.path()));
    let mut isolated_home = None;
    if policy.isolate_home {
        let root = root.expect("isolated gate environment scratch root");
        let home = root.join("gate-home");
        fs::create_dir_all(&home).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", home.display())),
            )
        })?;
        materialize_extension_inputs(&mut report, &home, &policy.extension_inputs)?;
        isolated_home = Some(home.clone());
        // Explicit Homeboy root overrides outrank HOME. An inherited (or
        // step-injected) controller `HOMEBOY_CONFIG_ROOT` / `HOMEBOY_DATA_DIR`
        // would let a gate running Homeboy's own tests rewrite the operator's
        // real config (#16106), so pin every one inside the gate home.
        for name in homeboy_core::paths::HOMEBOY_ROOT_ENV_VARS {
            let path = homeboy_core::paths::homeboy_root_env_default_under_home(name, &home)
                .expect("every Homeboy root env var has an in-home default");
            fs::create_dir_all(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", path.display())),
                )
            })?;
            set_isolated_environment_variable(&mut report, &mut values, name, path);
        }
        set_isolated_environment_variable(&mut report, &mut values, "HOME", home);
    } else if !policy.extension_inputs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_environment.extension_inputs",
            "extension inputs require HOME isolation",
            None,
            None,
        ));
    }
    if policy.isolate_xdg {
        let root = root.expect("isolated gate environment scratch root");
        for name in XDG_ENV_VARS {
            let path = root.join("gate-xdg").join(name.to_ascii_lowercase());
            fs::create_dir_all(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", path.display())),
                )
            })?;
            set_isolated_environment_variable(&mut report, &mut values, name, path);
        }
    }
    let input_store = if policy.data_inputs.is_empty() {
        None
    } else {
        Some(data_inputs::store_root()?)
    };
    for input in &policy.data_inputs {
        let path = input.path_in(input_store.as_deref().expect("declared inputs"))?;
        input.verify(&path)?;
        values.insert(input.env.clone(), path.display().to_string());
    }
    Ok(SelectedGateEnvironment {
        report,
        values,
        hydrate_rust_cache: policy.hydrate_rust_cache,
        isolate_download_cache: policy.isolate_home || policy.isolate_xdg,
        admitted_component_id: policy.admitted_component_id.clone(),
        isolated_home,
        extension_sources: policy.extension_sources.clone(),
        unprovisioned_extensions: Vec::new(),
        _rust_overlay: None,
        _cargo_target: None,
        _scratch: scratch,
    })
}

/// Homeboy-managed browser cache under the runner HOME. Checked before the
/// Playwright default so a runner provisioned by Homeboy keeps its pinned set.
pub(super) const HOMEBOY_PLAYWRIGHT_CACHE_SUBPATH: &str = ".cache/homeboy/playwright-browsers";

/// Locate the runner's installed Playwright browser cache, in order: the
/// runner's configured `PLAYWRIGHT_BROWSERS_PATH`, the Homeboy-managed cache,
/// then the Playwright default cache. Returns the resolved directory and a
/// replayable preserve source (`PLAYWRIGHT_BROWSERS_PATH` or `HOME/<subpath>`).
/// Absent caches resolve to `None`; that is not an error.
pub(super) fn playwright_cache_path(
    declared: Option<&Path>,
    host_home: Option<&Path>,
) -> Option<(PathBuf, String)> {
    const NAME: &str = "PLAYWRIGHT_BROWSERS_PATH";
    if let Some(path) = declared.map(Path::to_path_buf) {
        if path.is_dir() {
            return Some((path, NAME.to_string()));
        }
    }
    let home = host_home?;
    [HOMEBOY_PLAYWRIGHT_CACHE_SUBPATH, PLAYWRIGHT_CACHE_SUBPATH]
        .into_iter()
        .map(|subpath| (home.join(subpath), format!("HOME/{subpath}")))
        .find(|(cache, _)| cache.is_dir())
}

pub(super) fn preserve_playwright_cache(
    policy: &AgentTaskGateEnvironmentPolicy,
    declared: Option<&Path>,
    host_home: Option<&Path>,
    values: &mut BTreeMap<String, String>,
    report: &mut AgentTaskGateEnvironment,
) {
    const NAME: &str = "PLAYWRIGHT_BROWSERS_PATH";
    if (!policy.isolate_home && !policy.isolate_xdg)
        || policy.variables.contains_key(NAME)
        || policy.preserve.contains_key(NAME)
    {
        return;
    }
    preserve_playwright_cache_mapping(declared, host_home, values, report);
}

pub(super) fn preserve_playwright_cache_mapping(
    declared: Option<&Path>,
    host_home: Option<&Path>,
    values: &mut BTreeMap<String, String>,
    report: &mut AgentTaskGateEnvironment,
) {
    const NAME: &str = "PLAYWRIGHT_BROWSERS_PATH";
    if let Some((cache, source)) = playwright_cache_path(declared, host_home) {
        values.insert(NAME.to_string(), cache.display().to_string());
        // Record a replayable source (an environment name, optionally with a
        // HOME-relative suffix), never an absolute path: replay resolves
        // `preserve` entries through `preserved_environment_value`.
        report.preserved.insert(NAME.to_string(), source);
    }
}

/// Link extension-declared HOME-relative runtime directories from the host
/// HOME into the isolated gate HOME (#15836). Invalid declarations and
/// directories absent on this host are skipped; an existing destination is
/// left untouched. Returns the linked relative paths.
pub(super) fn link_declared_home_paths(
    host_home: &Path,
    gate_home: &Path,
    declared: &[String],
) -> Result<Vec<String>> {
    let mut linked = Vec::new();
    for path in declared {
        if homeboy_extension_contract_validate_home_path(path).is_err() {
            continue;
        }
        let source = host_home.join(path);
        if !source.is_dir() {
            continue;
        }
        let destination = gate_home.join(path);
        if fs::symlink_metadata(&destination).is_ok() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(error.to_string(), Some(parent.display().to_string()))
            })?;
        }
        let source = source.canonicalize().map_err(|error| {
            Error::internal_io(error.to_string(), Some(source.display().to_string()))
        })?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, &destination).map_err(|error| {
            Error::internal_io(error.to_string(), Some(destination.display().to_string()))
        })?;
        #[cfg(not(unix))]
        {
            let _ = (&source, &destination);
            continue;
        }
        linked.push(path.clone());
    }
    Ok(linked)
}

pub(super) fn homeboy_extension_contract_validate_home_path(path: &str) -> Result<()> {
    homeboy_extension_contract::manifest::validate_isolated_home_paths(std::slice::from_ref(
        &path.to_string(),
    ))
}

pub(super) struct LinkedRunnerExtension {
    pub(super) id: String,
    pub(super) source: PathBuf,
    pub(super) source_revision: Option<String>,
    pub(super) destination: PathBuf,
}

/// Symlink the requested installed extensions, plus the sibling shared-asset
/// directories they reference, from `host_extensions` into
/// `destination_root`. Ids in `skip` (explicit gate inputs) and ids not
/// installed on this host are left alone; a missing extension surfaces later
/// as the command's own "extension not installed" error.
pub(super) fn link_runner_extensions(
    host_extensions: &Path,
    destination_root: &Path,
    ids: &std::collections::BTreeSet<String>,
    skip: &std::collections::BTreeSet<String>,
) -> Result<Vec<LinkedRunnerExtension>> {
    let wanted: Vec<&String> = ids
        .iter()
        .filter(|id| !skip.contains(*id))
        .filter(|id| !id.is_empty() && !id.contains(['/', '\\']) && *id != "." && *id != "..")
        .filter(|id| host_extensions.join(id).is_dir())
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    fs::create_dir_all(destination_root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(destination_root.display().to_string()),
        )
    })?;
    let link = |source: &Path, destination: &Path| -> Result<()> {
        if fs::symlink_metadata(destination).is_ok() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(source, destination).map_err(|error| {
                Error::internal_io(error.to_string(), Some(destination.display().to_string()))
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (source, destination);
            Err(Error::invalid_argument(
                "gate_environment.linked_extensions",
                "linking runner extensions requires a Unix host",
            ))
        }
    };
    let mut linked = Vec::new();
    for id in wanted {
        let source = host_extensions.join(id).canonicalize().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(host_extensions.join(id).display().to_string()),
            )
        })?;
        let destination = destination_root.join(id);
        link(&source, &destination)?;
        linked.push(LinkedRunnerExtension {
            id: id.clone(),
            source_revision: homeboy_core::extension::lifecycle::read_source_revision_at(&source),
            source,
            destination,
        });
    }
    // Shared assets are sibling directories that are not themselves
    // extensions (no `<name>/<name>.json` manifest). Link them so extension
    // scripts resolving `../scripts` or `../dependency-adapters` still work.
    if let Ok(entries) = fs::read_dir(host_extensions) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || ids.contains(name) || skip.contains(name) {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() || path.join(format!("{name}.json")).is_file() {
                continue;
            }
            if let Ok(source) = path.canonicalize() {
                link(&source, &destination_root.join(name))?;
            }
        }
    }
    Ok(linked)
}

pub(super) fn materialize_extension_inputs(
    report: &mut AgentTaskGateEnvironment,
    home: &Path,
    inputs: &[AgentTaskGateExtensionInput],
) -> Result<()> {
    // Reused invocation roots must not retain files or links written by an earlier gate.
    let config = home.join(".config/homeboy");
    if !inputs.is_empty() {
        if fs::symlink_metadata(home.join(".config"))
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(Error::invalid_argument(
                "gate_environment.extension_inputs",
                "isolated gate config parent cannot be a symlink",
            ));
        }
        if let Ok(metadata) = fs::symlink_metadata(&config) {
            let result = if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(&config)
            } else {
                fs::remove_file(&config)
            };
            result.map_err(|error| {
                Error::internal_io(error.to_string(), Some(config.display().to_string()))
            })?;
        }
    }
    let extensions = home.join(".config/homeboy/extensions");
    let mut ids = std::collections::BTreeSet::new();
    let mut shared_targets: BTreeMap<PathBuf, String> = BTreeMap::new();
    for input in inputs {
        if input.id.trim().is_empty()
            || input.id.contains(['/', '\\'])
            || input.id == "."
            || input.id == ".."
        {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input ids must be non-empty path-free names",
                Some(input.id.clone()),
                None,
            ));
        }
        if !ids.insert(&input.id) {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input ids must be unique",
                Some(input.id.clone()),
                None,
            ));
        }
        let source = Path::new(&input.source);
        if !source.is_absolute() || !source.is_dir() {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input sources must be existing absolute directories",
                Some(input.source.clone()),
                None,
            ));
        }
        let resolved_source = source
            .canonicalize()
            .map_err(|error| Error::internal_io(error.to_string(), Some(input.source.clone())))?;
        let extension_identity = extension_tree_identity(&resolved_source)?;
        let mut shared_assets = Vec::new();
        for (path, asset_source) in
            homeboy_core::extension::lifecycle::shared_assets_for_extension_source(source)
        {
            let target = homeboy_core::extension::lifecycle::isolated_shared_asset_target(
                &home.join(".config/homeboy"),
                &path,
            )?;
            if inputs.iter().any(|selected| {
                let extension = extensions.join(&selected.id);
                target.starts_with(&extension) || extension.starts_with(&target)
            }) {
                return Err(Error::invalid_argument(
                    "gate_environment.extension_inputs",
                    "shared asset destinations cannot overlap selected extensions",
                ));
            }
            let asset_source = asset_source.canonicalize().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("resolve declared shared asset {path}")),
                )
            })?;
            let identity = extension_tree_identity(&asset_source)?;
            if shared_targets.iter().any(|(existing, expected)| {
                (target.starts_with(existing) || existing.starts_with(&target))
                    && (existing != &target || expected != &identity)
            }) {
                return Err(Error::invalid_argument(
                    "gate_environment.extension_inputs",
                    "declared shared asset destinations have conflicting identities or overlap",
                ));
            }
            shared_targets.insert(target.clone(), identity.clone());
            shared_assets.push(AgentTaskGateSharedAssetProvenance {
                path,
                source: asset_source.display().to_string(),
                identity,
                destination: target
                    .strip_prefix(home)
                    .expect("isolated shared asset")
                    .display()
                    .to_string(),
            });
        }
        shared_assets.sort_by(|left, right| left.path.cmp(&right.path));
        let identity = if shared_assets.is_empty() {
            extension_identity.clone()
        } else {
            let mut hasher = homeboy_engine_primitives::content_hash::sha256_hasher();
            hasher.update(b"homeboy/gate-extension-closure/v1\0");
            hasher.update(extension_identity.as_bytes());
            for asset in &shared_assets {
                hasher.update([0]);
                hasher.update(asset.path.as_bytes());
                hasher.update([0]);
                hasher.update(asset.identity.as_bytes());
            }
            format!("sha256:{:x}", hasher.finalize())
        };
        let source_revision = homeboy_core::extension::lifecycle::read_source_revision_at(source);
        if input
            .identity
            .as_deref()
            .is_some_and(|expected| expected != identity)
        {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input no longer matches the candidate gate identity",
                Some(input.id.clone()),
                None,
            ));
        }
        let destination = extensions.join(&input.id);
        copy_extension_input(&resolved_source, &destination)?;
        if extension_tree_identity(&destination)? != extension_identity {
            return Err(Error::invalid_argument(
                "gate_environment.extension_inputs",
                "extension input changed during materialization",
            ));
        }
        for asset in &shared_assets {
            let target = home.join(&asset.destination);
            copy_extension_input(Path::new(&asset.source), &target)?;
            if extension_tree_identity(&target)? != asset.identity {
                return Err(Error::invalid_argument(
                    "gate_environment.extension_inputs",
                    "shared asset changed during materialization",
                ));
            }
        }
        report
            .extension_inputs
            .push(AgentTaskGateExtensionInputProvenance {
                id: input.id.clone(),
                source: input.source.clone(),
                source_revision,
                identity,
                shared_assets,
                destination: destination
                    .strip_prefix(home)
                    .unwrap_or(&destination)
                    .display()
                    .to_string(),
            });
    }
    Ok(())
}

pub(super) fn extension_tree_identity(source: &Path) -> Result<String> {
    pub(super) fn visit(
        root: &Path,
        path: &Path,
        files: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<()> {
        for entry in fs::read_dir(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read extension input {}", path.display())),
            )
        })? {
            let entry = entry.map_err(|error| Error::internal_io(error.to_string(), None))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
            if metadata.file_type().is_symlink() {
                return Err(Error::validation_invalid_argument(
                    "gate_environment.extension_inputs",
                    "extension inputs cannot contain symlinks",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                files.push((
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                    fs::read(&path).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(path.display().to_string()))
                    })?,
                ));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(source, source, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = homeboy_engine_primitives::content_hash::sha256_hasher();
    for (path, bytes) in files {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(super) fn copy_extension_input(source: &Path, destination: &Path) -> Result<()> {
    // A private copy is the security boundary: the controller source is never
    // exposed to the gate. The copy is gate-owned because file permissions do
    // not provide a portable immutable boundary (and privileged processes can
    // always change them).
    homeboy_core::io::copy_tree::copy_tree(
        source,
        destination,
        "gate.extension_input.copy",
        homeboy_core::io::copy_tree::EntryPolicy::CopyRegularFilesOnly,
    )
}

pub(super) fn preserved_environment_value(source: &str) -> Result<String> {
    let (source_name, suffix) = source.split_once('/').unwrap_or((source, ""));
    let value = std::env::var(source_name).map_err(|_| {
        Error::validation_invalid_argument(
            "gate_environment.preserve",
            format!("declared gate environment source {source_name} is unavailable"),
            Some(source.to_string()),
            Some(vec![format!(
                "Set {source_name} before Cook or replace this mapping with an available toolchain home."
            )]),
        )
    })?;
    Ok(if suffix.is_empty() {
        value
    } else {
        PathBuf::from(value).join(suffix).display().to_string()
    })
}
