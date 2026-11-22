use super::*;

/// Prefix of the probe-failure sentence naming unprovisioned extensions.
pub(super) const MISSING_GATE_EXTENSIONS: &str =
    "the gate environment is missing declared extension";

pub(super) struct SelectedGateEnvironment {
    pub(super) report: AgentTaskGateEnvironment,
    pub(super) values: BTreeMap<String, String>,
    pub(super) hydrate_rust_cache: bool,
    pub(super) isolate_download_cache: bool,
    pub(super) admitted_component_id: Option<String>,
    /// Isolated HOME, when the policy isolates it.
    pub(super) isolated_home: Option<PathBuf>,
    pub(super) extension_sources: Vec<AgentTaskGateExtensionSource>,
    /// Declared extensions neither installed nor installable on this host,
    /// with the command that provisions each.
    pub(super) unprovisioned_extensions: Vec<(String, String)>,
    pub(super) _cargo_target: Option<homeboy_core::cleanup::ManagedCargoTarget>,
    pub(super) _scratch: Option<tempfile::TempDir>,
    pub(super) _rust_overlay: Option<tempfile::TempDir>,
}

impl SelectedGateEnvironment {
    /// Expose the admitted and checkout sibling components' linked extensions
    /// inside an isolated gate HOME (#15383, #17569).
    ///
    /// HOME isolation keeps host config and credentials out of gates, but it
    /// also hid every installed extension, so the canonical gate for an
    /// extension-backed component (`homeboy review test <component>`) could
    /// never run. Extensions are resolved on the host executing the gate (the
    /// Lab runner for Lab gates), so no controller path crosses the boundary.
    /// They are symlinked, not copied: installed extension trees carry
    /// `vendor/` and `node_modules/` (hundreds of MB) and the extension loader
    /// already follows linked extension directories. Sibling non-extension
    /// directories under `extensions/` are shared assets (e.g. `scripts/`,
    /// `dependency-adapters/`) referenced by extensions and are linked too.
    /// Ids already materialized from explicit `extension_inputs` are kept.
    /// Expose the admitted component's linked extensions and declared test
    /// secrets to the gate. Preflight probes and gate execution both call this,
    /// so a nested `homeboy review test` resolves identically in each
    /// (Extra-Chill/homeboy#16268).
    pub(super) fn configure_component_inputs(&mut self, cwd: &Path) -> Result<()> {
        self.register_admitted_component(cwd)?;
        self.configure_linked_extensions(cwd)?;
        self.carry_declared_secret_env(cwd);
        Ok(())
    }

    /// Register the admitted component inside an isolated gate HOME, at the
    /// checkout the gate runs in.
    ///
    /// HOME isolation hides the host's component registry, so the canonical
    /// gate `homeboy review test <component>` could only resolve a component
    /// whose repository ships a portable `homeboy.json`; a registry-only
    /// component failed with `component.not_found`. The host's registration
    /// (when it has one) is carried with `local_path` rebound to the gate
    /// checkout, so no host path crosses the boundary.
    pub(super) fn register_admitted_component(&mut self, cwd: &Path) -> Result<()> {
        let (Some(home), Some(id)) = (
            self.isolated_home.as_deref(),
            self.admitted_component_id.as_deref(),
        ) else {
            return Ok(());
        };
        let file = format!("{id}.json");
        let mut registration = homeboy_core::paths::components()
            .ok()
            .and_then(|host| fs::read(host.join(&file)).ok())
            .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| json!({}));
        // Gates materialize the repository, not just the admitted package.
        // Retain its repo-relative path before the isolated XDG root hides
        // the host registration. Root components and synthetic registrations
        // keep the existing checkout-root fallback.
        let component_path = registration
            .get("local_path")
            .and_then(serde_json::Value::as_str)
            .map(|path| PathBuf::from(shellexpand::tilde(path).into_owned()))
            .and_then(|path| {
                let path = path.canonicalize().ok()?;
                let root = homeboy_core::git::repo_root(&path)?;
                let relative = path.strip_prefix(root).ok()?;
                let workspace_root =
                    homeboy_core::git::repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
                Some(workspace_root.join(relative))
            })
            .unwrap_or_else(|| cwd.to_path_buf());
        registration["local_path"] = json!(component_path.to_string_lossy());
        let components = homeboy_core::paths::components_in_root(&home.join(".config/homeboy"));
        let io_error = |error: std::io::Error| {
            Error::internal_io(
                error.to_string(),
                Some("register gate component".to_string()),
            )
        };
        fs::create_dir_all(&components).map_err(io_error)?;
        let bytes = serde_json::to_vec_pretty(&registration)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        fs::write(components.join(file), bytes).map_err(io_error)
    }

    /// Carry the admitted component's declared test secrets into the gate.
    ///
    /// `replace` mode clears the child environment and HOME isolation hides
    /// Homeboy's secret mappings, so a nested `homeboy review test` could never
    /// see the secrets its test extension declares (for example a WordPress
    /// test database), even when the executing host (or the Lab runner, from
    /// its `secret_env` refs) supplied them to this process (#15824). Only the
    /// names the component's test capability declares are carried, only when
    /// present here, and the report records names, never values.
    pub(super) fn carry_declared_secret_env(&mut self, cwd: &Path) {
        let Ok(component) = homeboy_core::component::resolve_effective(
            self.admitted_component_id.as_deref(),
            Some(&cwd.to_string_lossy()),
            None,
        ) else {
            return;
        };
        let names = homeboy_core::extension::test::declared_secret_env_names(&component)
            .unwrap_or_default();
        self.carry_secret_env_names(&names, |name| std::env::var(name).ok());
    }

    pub(super) fn carry_secret_env_names(
        &mut self,
        names: &[String],
        lookup: impl Fn(&str) -> Option<String>,
    ) {
        for name in names {
            if self.values.contains_key(name) {
                continue;
            }
            if let Some(value) = lookup(name).filter(|value| !value.is_empty()) {
                self.values.insert(name.clone(), value);
                self.report.declared_secret_env.push(name.clone());
            }
        }
    }

    /// Install declared extensions this gate host lacks from the controller's
    /// recorded source and revision, through the ordinary extension install
    /// path. Ids that cannot be provisioned are remembered so a failed probe
    /// names them with the command that provisions them (#17769).
    pub(super) fn provision_missing_extensions(
        &mut self,
        host_extensions: &Path,
        ids: &std::collections::BTreeSet<String>,
        explicit: &std::collections::BTreeSet<String>,
    ) {
        for id in ids {
            if explicit.contains(id)
                || id.is_empty()
                || id.contains(['/', '\\'])
                || id == "."
                || id == ".."
                || host_extensions.join(id).is_dir()
            {
                continue;
            }
            let source = self
                .extension_sources
                .iter()
                .find(|source| &source.id == id);
            let installed = source.is_some_and(|source| {
                homeboy_core::extension::lifecycle::install_with_revision(
                    &source.source,
                    Some(id),
                    source.revision.as_deref(),
                )
                .is_ok()
            });
            if !installed {
                self.unprovisioned_extensions.push((
                    id.clone(),
                    AgentTaskGateExtensionSource::provision_command(id, source),
                ));
            }
        }
    }

    pub(super) fn configure_linked_extensions(&mut self, cwd: &Path) -> Result<()> {
        let Some(home) = self.isolated_home.clone() else {
            return Ok(());
        };
        let ids = declared_component_extension_ids(self.admitted_component_id.as_deref(), cwd);
        let Ok(host_extensions) = homeboy_core::paths::extensions() else {
            return Ok(());
        };
        let explicit: std::collections::BTreeSet<String> = self
            .report
            .extension_inputs
            .iter()
            .map(|input| input.id.clone())
            .collect();
        self.provision_missing_extensions(&host_extensions, &ids, &explicit);
        let destination_root = home.join(".config/homeboy/extensions");
        let report = link_runner_extensions(&host_extensions, &destination_root, &ids, &explicit)?;
        let host_home = std::env::var_os("HOME").map(PathBuf::from);
        for linked in report {
            let declared = homeboy_core::extension::catalog::load_extension(&linked.id)
                .map(|manifest| manifest.isolated_home_paths)
                .unwrap_or_default();
            let home_paths = match host_home.as_deref() {
                Some(host_home) => link_declared_home_paths(host_home, &home, &declared)?,
                None => Vec::new(),
            };
            self.report
                .linked_extensions
                .push(AgentTaskGateLinkedExtensionProvenance {
                    destination: linked
                        .destination
                        .strip_prefix(&home)
                        .unwrap_or(&linked.destination)
                        .display()
                        .to_string(),
                    id: linked.id,
                    source: linked.source.display().to_string(),
                    source_revision: linked.source_revision,
                    home_paths,
                });
        }
        Ok(())
    }

    pub(super) fn hydration_environment(
        &self,
    ) -> homeboy_core::deps::DependencyHydrationEnvironment {
        homeboy_core::deps::DependencyHydrationEnvironment {
            isolate_download_cache: self.isolate_download_cache,
            clear: self.report.mode == AgentTaskGateEnvironmentMode::Replace,
            remove: self
                .report
                .sanitized
                .iter()
                .map(|variable| variable.name.clone())
                .chain(
                    COOK_SUPERVISOR_PRIVATE_ENV_VARS
                        .iter()
                        .filter(|name| !self.values.contains_key(**name))
                        .map(|name| name.to_string()),
                )
                .chain(std::env::vars_os().filter_map(|(name, _)| {
                    let name = name.to_str()?;
                    (homeboy_core::runner_job_environment::is_runner_job_identity_env(name)
                        && !self.values.contains_key(name))
                    .then(|| name.to_string())
                }))
                .collect(),
            set: self.values.clone(),
        }
    }

    pub(super) fn apply(&self, process: &mut Command) {
        let environment = self.hydration_environment();
        if environment.clear {
            process.env_clear();
        }
        for name in environment.remove {
            process.env_remove(name);
        }
        process.envs(&environment.set);
    }

    pub(super) fn configure_rust_cache(&mut self, cwd: &Path) -> Result<()> {
        if !self.hydrate_rust_cache
            || !cwd.join("Cargo.lock").is_file()
            || !cwd.join("rust-toolchain.toml").is_file()
        {
            return Ok(());
        }

        let identity = rust_cache_identity(cwd)?;
        let lockfile_hash = format!(
            "{:x}",
            Sha256::digest(fs::read(cwd.join("Cargo.lock")).map_err(
                |error| Error::internal_io(
                    error.to_string(),
                    Some("read Rust cache lockfile".to_string())
                ),
            )?)
        );
        let root = homeboy_core::paths::homeboy_data()
            .map_err(|error| error.with_hint(rust_cache_repair_command()))?
            .join("controller-state/gate-rust-cache");
        let cache = root.join(&identity);
        ensure_safe_rust_cache_root(&root)?;
        create_private_dir(&cache).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create Rust gate cache {identity}")),
            )
            .with_hint(rust_cache_repair_command())
        })?;
        ensure_safe_rust_cache_root(&cache)?;

        let marker = cache.join("ready.json");
        let mut state = "hit";
        let mut wait_ms = None;
        let mut hydrate_ms = None;
        let toolchain_cargo_relative;
        let ready = valid_marker_toolchain_cargo(&marker, &identity, &cache)?;
        if let Some((relative, _)) =
            ready.filter(|(_, hash)| hash.as_deref() == Some(&lockfile_hash))
        {
            toolchain_cargo_relative = relative;
        } else {
            let lock_path = cache.join("hydrate.lock");
            let lock = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some("open Rust gate cache lock".to_string()),
                    )
                })?;
            let started = Instant::now();
            loop {
                let acquired = match lock.try_lock_exclusive() {
                    Ok(acquired) => acquired,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                    Err(error) => {
                        return Err(Error::internal_io(
                            error.to_string(),
                            Some("lock Rust gate cache".to_string()),
                        )
                        .with_hint(rust_cache_repair_command()));
                    }
                };
                if acquired {
                    break;
                }
                if started.elapsed() >= RUST_CACHE_LOCK_TIMEOUT {
                    return Err(Error::internal_io(
                        format!("timed out waiting for Rust gate cache {identity}"),
                        Some("hydrate Rust gate cache".to_string()),
                    )
                    .with_hint(rust_cache_repair_command()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let waited = started.elapsed().as_millis();
            let ready = valid_marker_toolchain_cargo(&marker, &identity, &cache)?;
            if let Some((relative, _)) = ready
                .as_ref()
                .filter(|(_, hash)| hash.as_deref() == Some(&lockfile_hash))
            {
                state = if waited > 0 { "wait" } else { "hit" };
                wait_ms = (waited > 0).then_some(waited);
                toolchain_cargo_relative = relative.clone();
            } else {
                let started = Instant::now();
                let relative = hydrate_rust_cache_with_timeout(
                    cwd,
                    &cache,
                    RUST_CACHE_HYDRATION_TIMEOUT,
                    ready.as_ref().map(|(relative, _)| relative.as_path()),
                )?;
                hydrate_ms = Some(started.elapsed().as_millis());
                fs::write(
                    &marker,
                    serde_json::to_vec(&json!({
                        "schema": RUST_CACHE_SCHEMA,
                        "identity": identity,
                        "lockfile_hash": lockfile_hash,
                        "toolchain_cargo_relative": relative,
                    }))
                    .expect("serialize Rust cache marker"),
                )
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some("write Rust gate cache marker".to_string()),
                    )
                    .with_hint(rust_cache_repair_command())
                })?;
                state = if ready.is_some() {
                    "fetched"
                } else {
                    "hydrated"
                };
                wait_ms = (waited > 0).then_some(waited);
                toolchain_cargo_relative = relative;
                // A new identity just landed; bound the cache to the most
                // recent ones (Extra-Chill/homeboy#15447). Best effort: an
                // eviction failure never fails the gate.
                if let Err(error) = evict_stale_rust_cache_identities(
                    &root,
                    &identity,
                    RUST_CACHE_RETAINED_IDENTITIES,
                ) {
                    homeboy_core::log_status!(
                        "gate",
                        "Rust gate cache eviction skipped: {}",
                        error.message
                    );
                }
            }
        }
        let home = self.values.get("HOME").map(PathBuf::from).ok_or_else(|| {
            Error::validation_invalid_argument(
                "gate_environment.isolate_home",
                "Rust gate caching requires HOME isolation",
                None,
                None,
            )
        })?;
        let overlay_root = home.join(".homeboy-rust-cache");
        fs::create_dir_all(&overlay_root)
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        let overlay_guard = tempfile::tempdir_in(&overlay_root)
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        let overlay = overlay_guard.path().to_path_buf();
        self._rust_overlay = Some(overlay_guard);
        copy_tree_preserving_safe_symlinks(&cache.join("cargo"), &overlay.join("cargo"))?;
        hydrate_gate_rustup(&cache.join("rustup"), &overlay.join("rustup"))?;
        self.values.insert(
            "CARGO_HOME".to_string(),
            overlay.join("cargo").display().to_string(),
        );
        self.values.insert(
            "RUSTUP_HOME".to_string(),
            overlay.join("rustup").display().to_string(),
        );
        let toolchain_bin = overlay
            .join("rustup")
            .join(&toolchain_cargo_relative)
            .parent()
            .expect("validated toolchain Cargo path has a parent")
            .to_path_buf();
        let inherited_path = self
            .values
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(toolchain_bin).chain(std::env::split_paths(&inherited_path)),
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("configure Rust gate toolchain PATH".to_string()),
            )
        })?;
        self.values
            .insert("PATH".to_string(), path.to_string_lossy().to_string());
        self.report.rust_cache = Some(AgentTaskGateRustCacheEvidence {
            identity,
            state: state.to_string(),
            wait_ms,
            hydrate_ms,
        });
        Ok(())
    }

    pub(super) fn configure_cargo_target(
        &mut self,
        cwd: &Path,
        override_enabled: Option<bool>,
    ) -> Result<()> {
        let admitted_component_id = self.admitted_component_id.clone();
        let enabled = override_enabled.unwrap_or_else(|| {
            homeboy_core::component::resolve_effective(
                admitted_component_id.as_deref(),
                Some(&cwd.to_string_lossy()),
                None,
            )
            .map(|component| component.managed_execution.shared_cargo_target)
            .unwrap_or(false)
        });
        if !enabled {
            return Ok(());
        }
        // A declared or inherited `CARGO_TARGET_DIR` describes the environment
        // this gate runs in, not a target an operator chose for it. Forwarding
        // it as a caller-owned explicit target would put two concurrent gates
        // in one Cargo directory, which is the contention this lease removes.
        // The value still reaches the child through `self.values`.
        let acquire_started = Instant::now();
        let tree = verdict::tree(cwd);
        let target = if let Some(tree) = tree {
            homeboy_core::cleanup::acquire_candidate_cargo_target_for_environment(
                "agent-task-gate",
                cwd,
                &self.values,
                &tree,
            )
        } else {
            homeboy_core::cleanup::acquire_managed_cargo_target_for_environment(
                "agent-task-gate",
                cwd,
                None,
                &self.values,
            )
        }
        .map_err(|error| placement::with_local_reserve_recovery(error))?;
        if target.resolution() != "candidate" {
            target.stamp_isolated_sources(cwd)?;
        }
        let acquire_ms = acquire_started.elapsed().as_millis();
        // Store sizing is evidence only. A concurrent gate may update the
        // shared target while this observation walks it.
        let bytes_before = target.size_bytes().ok();
        // Report the target this gate actually owns, rather than the shared
        // warm-cache identity from which it may have been seeded.
        let identity = target
            .target_dir()
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        self.values.insert(
            "CARGO_TARGET_DIR".to_string(),
            target.target_dir().to_string_lossy().to_string(),
        );
        self.report.cargo_target = Some(AgentTaskGateCargoTargetEvidence {
            acquire_ms: Some(acquire_ms),
            seed_ms: Some(target.seed_ms()),
            seed_bytes: Some(target.seed_bytes()),
            growth_bytes: None,
            path: target.target_dir().to_string_lossy().to_string(),
            resolution: target.resolution().to_string(),
            owner: target.evidence().owner,
            identity,
            state: bytes_before
                .map(|bytes| if bytes == 0 { "empty" } else { "seeded" }.to_string()),
            bytes_before,
            bytes_after: None,
        });
        self._cargo_target = Some(target);
        Ok(())
    }

    pub(super) fn finish_cargo_target(&mut self) -> Result<()> {
        let (Some(target), Some(evidence)) =
            (&mut self._cargo_target, &mut self.report.cargo_target)
        else {
            return Ok(());
        };
        evidence.bytes_after = target.size_bytes().ok();
        evidence.growth_bytes = evidence
            .bytes_after
            .zip(evidence.bytes_before)
            .map(|(after, before)| after.saturating_sub(before));
        target.publish()?;
        Ok(())
    }
}
