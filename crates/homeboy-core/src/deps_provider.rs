use crate::component::Component;
use crate::deps::{DependencyCommandResult, DependencyPackage, DependencyUpdateResult};
use crate::extension::{self, ExtensionCapability, ExtensionExecutionContext};
use crate::{paths, Error, Result};
use serde::Deserialize;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub(crate) struct ProviderDependencyStatus {
    pub package_manager: String,
    pub dependency_identities: Vec<String>,
    pub packages: Vec<DependencyPackage>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DependencyProviderSnapshot {
    pub identities: BTreeSet<String>,
    pub packages: Vec<DependencyPackage>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DependencyProviderContext<'a> {
    pub component: &'a Component,
    pub path: &'a Path,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DependencyProviderStatusRequest<'a> {
    pub context: DependencyProviderContext<'a>,
    pub package_filter: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DependencyProviderPackageRequest<'a> {
    pub package: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DependencyProviderUpdateRequest<'a> {
    pub context: DependencyProviderContext<'a>,
    pub package: &'a str,
    pub constraint: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyProviderCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl DependencyProviderCommand {
    pub fn new(program: impl Into<String>, args: Vec<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: cwd.into(),
        }
    }

    pub(crate) fn argv(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.clone())
            .collect()
    }
}

pub(crate) enum DependencyProvider {
    Manifest(ManifestDependencyProvider),
    Adapter(AdapterDependencyProvider),
    // Boxed: `ExtensionDependencyProvider` carries a full execution context and
    // is far larger than the other (zero-sized) variants, so storing it inline
    // would bloat every `DependencyProvider` value (clippy::large_enum_variant).
    Extension(Box<ExtensionDependencyProvider>),
    ComponentScript(ComponentScriptDependencyProvider),
}

impl DependencyProvider {
    pub(crate) fn status(
        &self,
        component: &Component,
        path: &Path,
        package_filter: Option<&str>,
    ) -> Result<ProviderDependencyStatus> {
        let request = DependencyProviderStatusRequest {
            context: DependencyProviderContext { component, path },
            package_filter,
        };
        match self {
            DependencyProvider::Manifest(provider) => provider.status(request),
            DependencyProvider::Adapter(provider) => provider.status(request),
            DependencyProvider::Extension(provider) => provider.status(request),
            DependencyProvider::ComponentScript(provider) => provider.status(request),
        }
    }

    pub(crate) fn handles_package(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
    ) -> Result<bool> {
        let _ = (component, path);
        let request = DependencyProviderPackageRequest { package };
        match self {
            DependencyProvider::Manifest(provider) => provider.handles_package(request),
            DependencyProvider::Adapter(provider) => provider.handles_package(request),
            DependencyProvider::Extension(provider) => provider.handles_package(request),
            DependencyProvider::ComponentScript(provider) => provider.handles_package(request),
        }
    }

    pub(crate) fn update(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
        constraint: Option<&str>,
    ) -> Result<DependencyUpdateResult> {
        let request = DependencyProviderUpdateRequest {
            context: DependencyProviderContext { component, path },
            package,
            constraint,
        };
        match self {
            DependencyProvider::Manifest(provider) => provider.update(request),
            DependencyProvider::Adapter(provider) => provider.update(request),
            DependencyProvider::Extension(provider) => provider.update(request),
            DependencyProvider::ComponentScript(provider) => provider.update(request),
        }
    }

    pub(crate) fn install(
        &self,
        component: &Component,
        path: &Path,
    ) -> Result<Option<DependencyCommandResult>> {
        let context = DependencyProviderContext { component, path };
        match self {
            DependencyProvider::Manifest(provider) => provider.install(context),
            DependencyProvider::Adapter(provider) => provider.install(context),
            DependencyProvider::ComponentScript(provider) => provider.install(context),
            DependencyProvider::Extension(provider) => provider.install(context),
        }
    }

    pub(crate) fn install_command(
        &self,
        component: &Component,
        path: &Path,
    ) -> Result<Option<DependencyProviderCommand>> {
        let context = DependencyProviderContext { component, path };
        match self {
            DependencyProvider::Manifest(provider) => provider.install_command(context),
            DependencyProvider::Adapter(provider) => provider.install_command(context),
            DependencyProvider::ComponentScript(provider) => provider.install_command(context),
            DependencyProvider::Extension(provider) => provider.install_command(context),
        }
    }
}

pub(crate) fn resolve_dependency_providers(
    component: &Component,
    path: &Path,
) -> Result<Vec<DependencyProvider>> {
    let providers = resolve_dependency_providers_optional(component, path)?;

    if providers.is_empty() {
        return Err(Error::validation_invalid_argument(
            "dependency_provider",
            format!("No dependency provider found for {}", path.display()),
            None,
            Some(vec![
                "Declare a dependency provider with homeboy-deps.json, an installed dependency adapter, a component deps script, or an extension deps provider".to_string(),
                "Core no longer detects built-in dependency providers; package ecosystems must be declared outside core".to_string(),
            ]),
        ));
    }

    Ok(providers)
}

/// Resolve the dependency providers a component/workspace exposes, returning an
/// empty vector when none are detected instead of erroring.
///
/// Setup orchestration treats "no provider" as a no-op, so it needs the empty
/// case without the actionable error that command-facing resolution raises.
pub(crate) fn resolve_dependency_providers_optional(
    component: &Component,
    path: &Path,
) -> Result<Vec<DependencyProvider>> {
    let mut providers = Vec::new();

    let local_adapter_providers = AdapterDependencyProvider::load_local(path)?;
    if !local_adapter_providers.is_empty() {
        return Ok(local_adapter_providers
            .into_iter()
            .map(DependencyProvider::Adapter)
            .collect());
    }

    if let Some(provider) = ManifestDependencyProvider::load(path)? {
        providers.push(DependencyProvider::Manifest(provider));
        return Ok(providers);
    }

    let adapter_providers = AdapterDependencyProvider::load(path)?;
    if !adapter_providers.is_empty() {
        return Ok(adapter_providers
            .into_iter()
            .map(DependencyProvider::Adapter)
            .collect());
    }

    if component.has_script(ExtensionCapability::Deps) {
        providers.push(DependencyProvider::ComponentScript(
            ComponentScriptDependencyProvider,
        ));
    }

    if component
        .extensions
        .as_ref()
        .map(|extensions| !extensions.is_empty())
        .unwrap_or(false)
    {
        if let Some(context) =
            extension::resolve_execution_context_if_available(component, ExtensionCapability::Deps)?
        {
            providers.push(DependencyProvider::Extension(Box::new(
                ExtensionDependencyProvider { context },
            )));
        }
    }

    Ok(providers)
}

pub(crate) fn dependency_provider_snapshot(
    component: &Component,
    path: &Path,
) -> Result<DependencyProviderSnapshot> {
    let mut snapshot = DependencyProviderSnapshot::default();
    snapshot.identities.insert(component.id.clone());
    snapshot
        .identities
        .extend(component.aliases.iter().cloned());

    let providers = match resolve_dependency_providers(component, path) {
        Ok(providers) => providers,
        Err(_) => return Ok(snapshot),
    };

    for provider in providers {
        let status = provider.status(component, path, None)?;
        snapshot.identities.extend(status.dependency_identities);
        snapshot.packages.extend(status.packages);
    }

    Ok(snapshot)
}

const DEPENDENCY_ADAPTER_INDEX: &str = "dependency-adapters/index.json";
const DEPENDENCY_ADAPTER_INDEX_SCHEMA: &str = "homeboy-extension/dependency-adapter-index/v1";
const DEPENDENCY_ADAPTER_MANIFEST_SCHEMA: &str = "homeboy-extension/dependency-adapter-manifest/v1";

#[derive(Debug, Clone)]
pub(crate) struct AdapterDependencyProvider {
    adapter: InstalledDependencyAdapter,
}

impl AdapterDependencyProvider {
    fn load_local(path: &Path) -> Result<Vec<Self>> {
        let manifest_path = path.join(DEPENDENCY_ADAPTER_MANIFEST);
        if !manifest_path.is_file() || !is_dependency_adapter_manifest(&manifest_path)? {
            return Ok(Vec::new());
        }
        let adapter = read_installed_dependency_adapter(&manifest_path)?;
        Ok(adapter
            .matches(path)
            .then(|| adapter.providers(path))
            .unwrap_or_default()
            .into_iter()
            .map(|adapter| Self { adapter })
            .collect())
    }

    fn load(path: &Path) -> Result<Vec<Self>> {
        let extensions_dir = paths::extensions()?;
        let Ok(entries) = fs::read_dir(extensions_dir) else {
            return Ok(Vec::new());
        };

        let mut adapters = Vec::new();
        let mut seen_manifests = HashSet::new();
        for entry in entries.flatten() {
            let index_path = entry.path().join(DEPENDENCY_ADAPTER_INDEX);
            if !index_path.is_file() {
                continue;
            }
            for manifest in read_dependency_adapter_index(&index_path)? {
                let manifest_path = index_path
                    .parent()
                    .expect("dependency adapter index has a parent")
                    .join(&manifest.path);
                if !seen_manifests.insert(manifest_path.clone()) {
                    continue;
                }
                let adapter = read_installed_dependency_adapter(&manifest_path)?;
                if adapter.id != manifest.id || adapter.ecosystem != manifest.ecosystem {
                    return Err(Error::validation_invalid_argument(
                        "dependency_adapter_index.manifests",
                        format!(
                            "Dependency adapter index entry '{}' does not match manifest '{}'",
                            manifest.id, adapter.id
                        ),
                        Some(manifest_path.display().to_string()),
                        None,
                    ));
                }
                if adapter.matches(path) {
                    adapters.extend(adapter.providers(path));
                }
            }
        }
        Ok(adapters
            .into_iter()
            .map(|adapter| Self { adapter })
            .collect())
    }

    fn status_with_filter(&self, package_filter: Option<&str>) -> Result<ProviderDependencyStatus> {
        if let Some(command) = &self.adapter.package_manager().commands.status {
            self.run(command, "status")?;
        }
        Ok(ProviderDependencyStatus {
            package_manager: self.adapter.package_manager().id.clone(),
            dependency_identities: self.adapter.package_identity()?,
            packages: self
                .adapter
                .packages()?
                .into_iter()
                .filter(|package| package_filter.is_none_or(|filter| filter == package.name))
                .collect(),
        })
    }

    fn command(&self, command: &AdapterCommand) -> DependencyProviderCommand {
        // Adapter commands are declared as shell command strings by the v1 contract.
        DependencyProviderCommand::new(
            "sh",
            vec!["-c".to_string(), command.command.clone()],
            self.adapter.project_path.clone(),
        )
    }

    fn run(&self, command: &AdapterCommand, operation: &str) -> Result<DependencyCommandResult> {
        if command.requires_lockfile
            && !self
                .adapter
                .lockfile_priority
                .iter()
                .any(|lockfile| self.adapter.project_path.join(lockfile).exists())
        {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                format!(
                    "Dependency adapter '{}' requires one of its declared lockfiles before {}",
                    self.adapter.package_manager().id,
                    operation
                ),
                None,
                None,
            ));
        }
        run_adapter_command(
            &self.command(command),
            operation,
            &command.success_exit_codes,
        )
    }

    fn status(
        &self,
        request: DependencyProviderStatusRequest<'_>,
    ) -> Result<ProviderDependencyStatus> {
        self.status_with_filter(request.package_filter)
    }

    fn handles_package(&self, request: DependencyProviderPackageRequest<'_>) -> Result<bool> {
        Ok(self
            .adapter
            .packages()?
            .iter()
            .any(|package| package.name == request.package)
            || self.adapter.package_manager().commands.update.is_some())
    }

    fn update(
        &self,
        request: DependencyProviderUpdateRequest<'_>,
    ) -> Result<DependencyUpdateResult> {
        let Some(command) = &self.adapter.package_manager().commands.update else {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                format!(
                    "Dependency adapter '{}' does not define an update command",
                    self.adapter.package_manager().id
                ),
                Some(self.adapter.package_manager().id.clone()),
                None,
            ));
        };
        let before = self
            .status_with_filter(Some(request.package))?
            .packages
            .into_iter()
            .next();
        let result = self.run(command, "update")?;
        let after = self
            .status_with_filter(Some(request.package))?
            .packages
            .into_iter()
            .next();
        Ok(dependency_update_result(
            request,
            self.adapter.package_manager().id.clone(),
            result,
            before,
            after,
        ))
    }

    fn install(
        &self,
        _context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyCommandResult>> {
        let Some(command) = &self.adapter.package_manager().commands.install else {
            return Ok(None);
        };
        Ok(Some(self.run(command, "install")?))
    }

    fn install_command(
        &self,
        _context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyProviderCommand>> {
        Ok(self
            .adapter
            .package_manager()
            .commands
            .install
            .as_ref()
            .map(|command| self.command(command)))
    }
}

fn is_dependency_adapter_manifest(path: &Path) -> Result<bool> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw))
    })?;
    Ok(value.get("schema").and_then(serde_json::Value::as_str)
        == Some(DEPENDENCY_ADAPTER_MANIFEST_SCHEMA))
}

#[derive(Debug, Deserialize)]
struct DependencyAdapterIndex {
    schema: String,
    manifests: Vec<DependencyAdapterIndexEntry>,
}

#[derive(Debug, Deserialize)]
struct DependencyAdapterIndexEntry {
    id: String,
    ecosystem: String,
    path: PathBuf,
}

fn read_dependency_adapter_index(path: &Path) -> Result<Vec<DependencyAdapterIndexEntry>> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    let index: DependencyAdapterIndex = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw))
    })?;
    if index.schema != DEPENDENCY_ADAPTER_INDEX_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "dependency_adapter_index.schema",
            format!(
                "Unsupported dependency adapter index schema '{}'",
                index.schema
            ),
            Some(index.schema),
            None,
        ));
    }
    for entry in &index.manifests {
        if entry.id.trim().is_empty()
            || entry.ecosystem.trim().is_empty()
            || entry.path.as_os_str().is_empty()
            || entry.path.is_absolute()
            || entry
                .path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(Error::validation_invalid_argument(
                "dependency_adapter_index.manifests",
                "Dependency adapter index entries require an id, ecosystem, and relative path"
                    .to_string(),
                None,
                None,
            ));
        }
    }
    Ok(index.manifests)
}

#[derive(Debug, Clone, Deserialize)]
struct InstalledDependencyAdapter {
    schema: String,
    id: String,
    version: u64,
    ecosystem: String,
    project_signals: AdapterProjectSignals,
    #[serde(default)]
    lockfile_priority: Vec<String>,
    #[serde(default)]
    package_managers: Vec<AdapterPackageManager>,
    #[serde(skip)]
    project_path: PathBuf,
}

impl InstalledDependencyAdapter {
    fn matches(&self, path: &Path) -> bool {
        let mut matched = self
            .project_signals
            .root_files
            .iter()
            .map(|file| path.join(file).exists());
        match self.project_signals.root_match.as_deref().unwrap_or("all") {
            "any" => matched.any(|matched| matched),
            _ => matched.all(|matched| matched),
        }
    }

    fn providers(&self, path: &Path) -> Vec<Self> {
        let mut package_managers = self.package_managers.clone();
        package_managers.sort_by_key(|manager| manager.selection.priority);
        let selected = package_managers
            .iter()
            .find(|manager| manager.selection_matches(path))
            .or_else(|| {
                package_managers
                    .iter()
                    .find(|manager| manager.selection.default)
            });
        selected
            .cloned()
            .map(|package_manager| {
                let mut adapter = self.clone();
                adapter.project_path = path.to_path_buf();
                adapter.package_managers = vec![package_manager];
                adapter
            })
            .into_iter()
            .collect()
    }

    fn package_manager(&self) -> &AdapterPackageManager {
        &self.package_managers[0]
    }

    fn package_identity(&self) -> Result<Vec<String>> {
        let Some(identity) = &self.package_manager().package_identity else {
            return Ok(Vec::new());
        };
        let value = read_adapter_json(&self.project_path.join(&identity.manifest))?;
        Ok(value
            .get(&identity.name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .into_iter()
            .collect())
    }

    fn packages(&self) -> Result<Vec<DependencyPackage>> {
        let Some(identity) = &self.package_manager().package_identity else {
            return Ok(Vec::new());
        };
        let value = read_adapter_json(&self.project_path.join(&identity.manifest))?;
        let mut packages = Vec::new();
        for section in &identity.dependencies {
            let Some(dependencies) = value.get(section).and_then(serde_json::Value::as_object)
            else {
                continue;
            };
            packages.extend(dependencies.iter().filter_map(|(name, constraint)| {
                constraint.as_str().map(|constraint| DependencyPackage {
                    name: name.clone(),
                    manifest_section: Some(section.clone()),
                    constraint: Some(constraint.to_string()),
                    locked_version: None,
                    locked_reference: None,
                })
            }));
        }
        Ok(packages)
    }
}

fn read_installed_dependency_adapter(path: &Path) -> Result<InstalledDependencyAdapter> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    let adapter: InstalledDependencyAdapter = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw))
    })?;
    if adapter.schema != DEPENDENCY_ADAPTER_MANIFEST_SCHEMA
        || adapter.id.trim().is_empty()
        || adapter.ecosystem.trim().is_empty()
        || adapter.version == 0
        || adapter.project_signals.root_files.is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "dependency_adapter_manifest",
            "Dependency adapter manifests require a supported schema, id, version, ecosystem, and project root signals".to_string(),
            None,
            None,
        ));
    }
    if !matches!(
        adapter.project_signals.root_match.as_deref(),
        None | Some("any") | Some("all")
    ) {
        return Err(Error::validation_invalid_argument(
            "dependency_adapter_manifest.project_signals.root_match",
            "Dependency adapter root_match must be 'any' or 'all'".to_string(),
            None,
            None,
        ));
    }
    for manager in &adapter.package_managers {
        if manager.id.trim().is_empty()
            || !matches!(
                manager.selection.search.as_deref(),
                None | Some("project-root") | Some("upward")
            )
            || [
                manager.commands.status.as_ref(),
                manager.commands.install.as_ref(),
                manager.commands.update.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|command| command.command.trim().is_empty())
        {
            return Err(Error::validation_invalid_argument(
                "dependency_adapter_manifest.package_managers",
                "Dependency adapter package managers require an id, supported search mode, and non-empty commands".to_string(),
                None,
                None,
            ));
        }
    }
    Ok(adapter)
}

#[derive(Debug, Clone, Deserialize)]
struct AdapterProjectSignals {
    root_files: Vec<String>,
    root_match: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AdapterPackageManager {
    id: String,
    selection: AdapterSelection,
    #[serde(default)]
    commands: AdapterCommands,
    #[serde(default)]
    package_identity: Option<AdapterPackageIdentity>,
}

impl AdapterPackageManager {
    fn selection_matches(&self, path: &Path) -> bool {
        self.selection
            .files
            .iter()
            .any(|file| match self.selection.search.as_deref() {
                Some("upward") => path
                    .ancestors()
                    .any(|directory| directory.join(file).exists()),
                _ => path.join(file).exists(),
            })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AdapterSelection {
    priority: u64,
    #[serde(default)]
    files: Vec<String>,
    search: Option<String>,
    #[serde(default)]
    default: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AdapterCommands {
    status: Option<AdapterCommand>,
    install: Option<AdapterCommand>,
    update: Option<AdapterCommand>,
}

#[derive(Debug, Clone, Deserialize)]
struct AdapterCommand {
    command: String,
    #[serde(default)]
    success_exit_codes: Vec<i32>,
    #[serde(default)]
    requires_lockfile: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct AdapterPackageIdentity {
    manifest: PathBuf,
    name: String,
    dependencies: Vec<String>,
}

fn read_adapter_json(path: &Path) -> Result<serde_json::Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw)))
}

const DEPENDENCY_ADAPTER_MANIFEST: &str = "homeboy-deps.json";

#[derive(Debug, Clone)]
pub(crate) struct ManifestDependencyProvider {
    manifest: DependencyAdapterManifest,
}

impl ManifestDependencyProvider {
    fn load(path: &Path) -> Result<Option<Self>> {
        let manifest_path = path.join(DEPENDENCY_ADAPTER_MANIFEST);
        if !manifest_path.is_file() {
            return Ok(None);
        }

        let manifest = read_dependency_adapter_manifest(&manifest_path)?;
        Ok(Some(Self { manifest }))
    }

    fn command(
        &self,
        command: &DependencyAdapterCommand,
        context: DependencyProviderContext<'_>,
        package: Option<&str>,
        constraint: Option<&str>,
    ) -> DependencyProviderCommand {
        let mut argv = command.argv.clone();
        for arg in &mut argv {
            *arg = expand_manifest_command_arg(arg, package, constraint);
        }
        argv.retain(|arg| !arg.is_empty());
        let program = argv.first().cloned().unwrap_or_default();
        let args = argv.into_iter().skip(1).collect();
        let cwd = command
            .cwd
            .as_ref()
            .map(|cwd| context.path.join(cwd))
            .unwrap_or_else(|| context.path.to_path_buf());
        DependencyProviderCommand::new(program, args, cwd)
    }

    fn status_with_filter(&self, package_filter: Option<&str>) -> ProviderDependencyStatus {
        ProviderDependencyStatus {
            package_manager: self.manifest.provider.clone(),
            dependency_identities: self.manifest.dependency_identities.clone(),
            packages: self
                .manifest
                .packages
                .iter()
                .filter(|package| {
                    package_filter
                        .map(|filter| filter == package.name)
                        .unwrap_or(true)
                })
                .cloned()
                .collect(),
        }
    }
}

impl ManifestDependencyProvider {
    fn status(
        &self,
        request: DependencyProviderStatusRequest<'_>,
    ) -> Result<ProviderDependencyStatus> {
        Ok(self.status_with_filter(request.package_filter))
    }

    fn handles_package(&self, request: DependencyProviderPackageRequest<'_>) -> Result<bool> {
        Ok(self
            .manifest
            .packages
            .iter()
            .any(|package| package.name == request.package)
            || self.manifest.commands.update.is_some())
    }

    fn update(
        &self,
        request: DependencyProviderUpdateRequest<'_>,
    ) -> Result<DependencyUpdateResult> {
        let Some(command) = &self.manifest.commands.update else {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                format!(
                    "Dependency adapter '{}' does not define an update command",
                    self.manifest.provider
                ),
                Some(self.manifest.provider.clone()),
                None,
            ));
        };
        let before = self
            .status_with_filter(Some(request.package))
            .packages
            .into_iter()
            .next();
        let command = self.command(
            command,
            request.context,
            Some(request.package),
            request.constraint,
        );
        let result = run_dependency_provider_command(&command, "command")?;
        let after = self
            .status_with_filter(Some(request.package))
            .packages
            .into_iter()
            .next();

        Ok(dependency_update_result(
            request,
            self.manifest.provider.clone(),
            result,
            before,
            after,
        ))
    }

    fn install(
        &self,
        context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyCommandResult>> {
        let Some(command) = &self.manifest.commands.install else {
            return Ok(None);
        };
        let command = self.command(command, context, None, None);
        Ok(Some(run_dependency_provider_command(&command, "install")?))
    }

    fn install_command(
        &self,
        context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyProviderCommand>> {
        Ok(self
            .manifest
            .commands
            .install
            .as_ref()
            .map(|command| self.command(command, context, None, None)))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DependencyAdapterManifest {
    provider: String,
    #[serde(default)]
    dependency_identities: Vec<String>,
    #[serde(default)]
    packages: Vec<DependencyPackage>,
    #[serde(default)]
    commands: DependencyAdapterCommands,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DependencyAdapterCommands {
    install: Option<DependencyAdapterCommand>,
    update: Option<DependencyAdapterCommand>,
}

#[derive(Debug, Clone, Deserialize)]
struct DependencyAdapterCommand {
    argv: Vec<String>,
    cwd: Option<PathBuf>,
}

fn read_dependency_adapter_manifest(path: &Path) -> Result<DependencyAdapterManifest> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    let manifest: DependencyAdapterManifest = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw))
    })?;
    if manifest.provider.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider",
            "Dependency adapter manifest provider must not be empty".to_string(),
            None,
            None,
        ));
    }
    for command in [
        manifest.commands.install.as_ref(),
        manifest.commands.update.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if command.argv.is_empty() {
            return Err(Error::validation_invalid_argument(
                "commands.argv",
                "Dependency adapter command argv must not be empty".to_string(),
                None,
                None,
            ));
        }
    }
    Ok(manifest)
}

fn expand_manifest_command_arg(
    arg: &str,
    package: Option<&str>,
    constraint: Option<&str>,
) -> String {
    arg.replace("{package}", package.unwrap_or_default())
        .replace("{constraint}", constraint.unwrap_or_default())
}

pub(crate) struct ComponentScriptDependencyProvider;

impl ComponentScriptDependencyProvider {
    fn status(
        &self,
        request: DependencyProviderStatusRequest<'_>,
    ) -> Result<ProviderDependencyStatus> {
        let mut args = vec!["status".to_string()];
        if let Some(package_filter) = request.package_filter {
            args.push(package_filter.to_string());
        }
        let output =
            run_component_deps_script(request.context.component, request.context.path, &args)?;
        let status: ExtensionStatusOutput = parse_extension_output(&output.stdout, "deps status")?;

        Ok(ProviderDependencyStatus {
            package_manager: status.package_manager,
            dependency_identities: status.dependency_identities,
            packages: status.packages,
        })
    }

    fn handles_package(&self, _request: DependencyProviderPackageRequest<'_>) -> Result<bool> {
        Ok(true)
    }

    fn update(
        &self,
        request: DependencyProviderUpdateRequest<'_>,
    ) -> Result<DependencyUpdateResult> {
        let component = request.context.component;
        let path = request.context.path;
        let package = request.package;
        let mut args = vec!["update".to_string(), package.to_string()];
        if let Some(constraint) = request.constraint {
            args.push(constraint.to_string());
        }
        let output = run_component_deps_script(component, path, &args)?;
        let mut result: DependencyUpdateResult =
            parse_extension_output(&output.stdout, "deps update")?;
        result.component_id = component.id.clone();
        result.component_path = path.display().to_string();
        result.package = package.to_string();
        result.requested_constraint = request.constraint.map(str::to_string);
        result.stdout = output.stdout;
        result.stderr = output.stderr;
        result.install = None;
        result.rebuild = None;
        Ok(result)
    }

    fn install(
        &self,
        context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyCommandResult>> {
        let args = vec!["install".to_string()];
        let output = run_component_deps_script(context.component, context.path, &args)?;
        Ok(Some(DependencyCommandResult {
            command: component_deps_script_command(context.component, &args),
            skipped: false,
            status: Some(output.exit_code),
            stdout: output.stdout,
            stderr: output.stderr,
        }))
    }

    fn install_command(
        &self,
        _context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyProviderCommand>> {
        Ok(None)
    }
}

pub(crate) struct ExtensionDependencyProvider {
    context: ExtensionExecutionContext,
}

impl ExtensionDependencyProvider {
    fn status(
        &self,
        request: DependencyProviderStatusRequest<'_>,
    ) -> Result<ProviderDependencyStatus> {
        let mut args = vec!["status".to_string()];
        if let Some(package_filter) = request.package_filter {
            args.push(package_filter.to_string());
        }
        let output = self.run(request.context.component, request.context.path, &args)?;
        let status: ExtensionStatusOutput = parse_extension_output(&output.stdout, "deps status")?;

        Ok(ProviderDependencyStatus {
            package_manager: status.package_manager,
            dependency_identities: status.dependency_identities,
            packages: status.packages,
        })
    }

    fn handles_package(&self, _request: DependencyProviderPackageRequest<'_>) -> Result<bool> {
        Ok(true)
    }

    fn update(
        &self,
        request: DependencyProviderUpdateRequest<'_>,
    ) -> Result<DependencyUpdateResult> {
        let component = request.context.component;
        let path = request.context.path;
        let package = request.package;
        let mut args = vec!["update".to_string(), package.to_string()];
        if let Some(constraint) = request.constraint {
            args.push(constraint.to_string());
        }
        let output = self.run(component, path, &args)?;
        let mut result: DependencyUpdateResult =
            parse_extension_output(&output.stdout, "deps update")?;
        result.component_id = component.id.clone();
        result.component_path = path.display().to_string();
        result.package = package.to_string();
        result.requested_constraint = request.constraint.map(str::to_string);
        result.stdout = output.stdout;
        result.stderr = output.stderr;
        result.install = None;
        result.rebuild = None;
        Ok(result)
    }

    fn install(
        &self,
        context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyCommandResult>> {
        let args = vec!["install".to_string()];
        let output = self.run(context.component, context.path, &args)?;
        Ok(Some(DependencyCommandResult {
            command: extension_deps_command(&args),
            skipped: false,
            status: Some(output.exit_code),
            stdout: output.stdout,
            stderr: output.stderr,
        }))
    }

    fn install_command(
        &self,
        context: DependencyProviderContext<'_>,
    ) -> Result<Option<DependencyProviderCommand>> {
        let args = vec!["install-command".to_string()];
        let output = self.run(context.component, context.path, &args)?;
        let plan: ExtensionInstallCommandOutput =
            parse_extension_output(&output.stdout, "deps install-command")?;
        if plan.command.is_empty() {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                "Dependency provider install-command returned an empty command".to_string(),
                None,
                None,
            ));
        }
        Ok(Some(DependencyProviderCommand::new(
            plan.command.first().cloned().unwrap_or_default(),
            plan.command.into_iter().skip(1).collect(),
            context.path,
        )))
    }
}

impl ExtensionDependencyProvider {
    fn run(
        &self,
        component: &Component,
        path: &Path,
        args: &[String],
    ) -> Result<crate::extension::RunnerOutput> {
        crate::extension::ExtensionRunner::for_context(self.context.clone())
            .component(component.clone())
            .path_override(Some(path.display().to_string()))
            .working_dir(&path.display().to_string())
            .passthrough(false)
            .script_args(args)
            .run()
    }
}

fn run_component_deps_script(
    component: &Component,
    path: &Path,
    args: &[String],
) -> Result<crate::component_script_provider::ComponentScriptOutput> {
    let output = crate::component_script_provider::run_component_scripts_with_env(
        component,
        ExtensionCapability::Deps,
        path,
        false,
        &[],
        args,
    )?;
    if !output.success {
        return Err(Error::validation_invalid_argument(
            "dependency_provider",
            format!(
                "Dependency provider command failed with status {}: {}",
                output.exit_code,
                first_non_empty_line(&output.stderr)
                    .or_else(|| first_non_empty_line(&output.stdout))
                    .unwrap_or("no output")
            ),
            None,
            Some(vec![format!(
                "Run the component deps script manually in {}",
                path.display()
            )]),
        ));
    }
    Ok(output)
}

fn component_deps_script_command(component: &Component, args: &[String]) -> Vec<String> {
    let mut command = vec!["scripts.deps".to_string()];
    command.extend(
        component
            .script_commands(ExtensionCapability::Deps)
            .iter()
            .cloned(),
    );
    command.extend(args.iter().cloned());
    command
}

fn extension_deps_command(args: &[String]) -> Vec<String> {
    let mut command = vec!["extension.deps".to_string()];
    command.extend(args.iter().cloned());
    command
}

fn dependency_update_result(
    request: DependencyProviderUpdateRequest<'_>,
    provider: String,
    result: DependencyCommandResult,
    before: Option<DependencyPackage>,
    after: Option<DependencyPackage>,
) -> DependencyUpdateResult {
    DependencyUpdateResult {
        component_id: request.context.component.id.clone(),
        component_path: request.context.path.display().to_string(),
        package_manager: provider,
        package: request.package.to_string(),
        requested_constraint: request.constraint.map(str::to_string),
        command: result.command,
        before,
        after,
        stdout: result.stdout,
        stderr: result.stderr,
        install: None,
        rebuild: None,
    }
}

pub(crate) fn run_dependency_provider_command(
    command: &DependencyProviderCommand,
    operation: &str,
) -> Result<DependencyCommandResult> {
    let output = Command::new(&command.program)
        .args(&command.args)
        .current_dir(&command.cwd)
        .output()
        .map_err(|e| {
            Error::internal_io(e.to_string(), Some("run dependency provider".to_string()))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = output.status.code();
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "dependency_provider",
            format!(
                "Dependency provider {} failed with status {}: {}",
                operation,
                output.status,
                first_non_empty_line(&stderr)
                    .or_else(|| first_non_empty_line(&stdout))
                    .unwrap_or("no output")
            ),
            None,
            Some(vec![format!(
                "Run manually in {}: {}",
                command.cwd.display(),
                command.argv().join(" ")
            )]),
        ));
    }

    Ok(DependencyCommandResult {
        command: command.argv(),
        skipped: false,
        status,
        stdout,
        stderr,
    })
}

fn run_adapter_command(
    command: &DependencyProviderCommand,
    operation: &str,
    success_exit_codes: &[i32],
) -> Result<DependencyCommandResult> {
    let output = Command::new(&command.program)
        .args(&command.args)
        .current_dir(&command.cwd)
        .output()
        .map_err(|e| {
            Error::internal_io(e.to_string(), Some("run dependency adapter".to_string()))
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let status = output.status.code();
    let allowed = success_exit_codes
        .is_empty()
        .then(|| output.status.success())
        .unwrap_or_else(|| status.is_some_and(|code| success_exit_codes.contains(&code)));
    if !allowed {
        return Err(Error::validation_invalid_argument(
            "dependency_provider",
            format!(
                "Dependency adapter {} failed with status {}: {}",
                operation,
                output.status,
                first_non_empty_line(&stderr)
                    .or_else(|| first_non_empty_line(&stdout))
                    .unwrap_or("no output")
            ),
            None,
            Some(vec![format!(
                "Run manually in {}: {}",
                command.cwd.display(),
                command.argv().join(" ")
            )]),
        ));
    }
    Ok(DependencyCommandResult {
        command: command.argv(),
        skipped: false,
        status,
        stdout,
        stderr,
    })
}

#[derive(Debug, Deserialize)]
struct ExtensionStatusOutput {
    package_manager: String,
    #[serde(default)]
    dependency_identities: Vec<String>,
    #[serde(default)]
    packages: Vec<DependencyPackage>,
}

#[derive(Debug, Deserialize)]
struct ExtensionInstallCommandOutput {
    command: Vec<String>,
}

fn parse_extension_output<T: for<'de> Deserialize<'de>>(stdout: &str, action: &str) -> Result<T> {
    serde_json::from_str(stdout).map_err(|e| {
        Error::validation_invalid_json(e, Some(action.to_string()), Some(stdout.to_string()))
    })
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().find(|line| !line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Component;
    use crate::extension::ExtensionCapability;
    use crate::paths;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn extension_provider_reports_runner_safe_install_command() {
        let dir = tempdir().unwrap();
        let extension_path = dir.path().join("fixture-deps");
        let component_path = dir.path().join("component");
        fs::create_dir_all(&extension_path).unwrap();
        fs::create_dir_all(&component_path).unwrap();
        fs::write(
            extension_path.join("fixture-deps.json"),
            r#"{
                "name": "Fixture Deps",
                "version": "0.0.0",
                "deps": { "extension_script": "deps.sh" }
            }"#,
        )
        .unwrap();
        fs::write(
            extension_path.join("deps.sh"),
            r#"#!/bin/sh
case "$1" in
  status) printf '{"package_manager":"fixture","packages":[]}\n' ;;
  install-command) printf '{"command":["fixture-pm","install","--locked"]}\n' ;;
  *) exit 64 ;;
esac
"#,
        )
        .unwrap();
        make_executable(&extension_path.join("deps.sh"));

        let component = Component::new(
            "fixture".to_string(),
            component_path.display().to_string(),
            String::new(),
            None,
        );
        let provider = ExtensionDependencyProvider {
            context: crate::extension::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Deps,
                extension_id: "fixture-deps".to_string(),
                extension_path,
                script_path: "deps.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
            },
        };

        let status = provider
            .status(DependencyProviderStatusRequest {
                context: DependencyProviderContext {
                    component: &component,
                    path: &component_path,
                },
                package_filter: None,
            })
            .unwrap();
        assert_eq!(status.package_manager, "fixture");

        let command = provider
            .install_command(DependencyProviderContext {
                component: &component,
                path: &component_path,
            })
            .unwrap()
            .unwrap();
        assert_eq!(command.argv(), vec!["fixture-pm", "install", "--locked"]);
        assert_eq!(command.cwd, component_path);
    }

    #[test]
    fn reads_valid_dependency_adapter_index_and_rejects_malformed_indexes() {
        let dir = tempdir().unwrap();
        let valid = dir.path().join("index.json");
        fs::write(
            &valid,
            r#"{
                "schema": "homeboy-extension/dependency-adapter-index/v1",
                "manifests": [{ "id": "fixture", "ecosystem": "fixture", "path": "fixture.json" }]
            }"#,
        )
        .unwrap();
        let entries = read_dependency_adapter_index(&valid).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("fixture.json"));

        let malformed = dir.path().join("malformed.json");
        fs::write(&malformed, "{ not json }").unwrap();
        assert!(read_dependency_adapter_index(&malformed).is_err());
    }

    #[test]
    fn registry_adapter_precedes_scripts_but_scripts_resolve_without_a_registry() {
        crate::test_support::with_isolated_home(|_| {
            let dir = tempdir().unwrap();
            let project = dir.path().join("project");
            fs::create_dir_all(&project).unwrap();
            fs::write(
                project.join("fixture.json"),
                r#"{"name":"fixture/project"}"#,
            )
            .unwrap();
            let mut component = Component::new(
                "fixture".to_string(),
                project.display().to_string(),
                String::new(),
                None,
            );
            component.scripts = Some(crate::component::ComponentScriptsConfig {
                deps: vec!["true".to_string()],
                ..Default::default()
            });

            let providers = resolve_dependency_providers_optional(&component, &project).unwrap();
            assert!(matches!(
                providers.as_slice(),
                [DependencyProvider::ComponentScript(_)]
            ));

            fs::write(
                project.join(DEPENDENCY_ADAPTER_MANIFEST),
                r#"{
                    "schema": "homeboy-extension/dependency-adapter-manifest/v1",
                    "id": "fixture",
                    "version": 1,
                    "ecosystem": "fixture",
                    "project_signals": { "root_files": ["fixture.json"] },
                    "capabilities": {},
                    "package_managers": [{
                        "id": "fixture-pm",
                        "selection": { "priority": 1, "default": true },
                        "install": { "intent": "install" },
                        "commands": {},
                        "outputs": [{ "kind": "directory", "path": "deps" }]
                    }]
                }"#,
            )
            .unwrap();
            let providers = resolve_dependency_providers_optional(&component, &project).unwrap();
            assert!(matches!(
                providers.as_slice(),
                [DependencyProvider::Adapter(_)]
            ));
            fs::remove_file(project.join(DEPENDENCY_ADAPTER_MANIFEST)).unwrap();

            let adapter_dir = paths::extensions()
                .unwrap()
                .join("fixture-extension/dependency-adapters");
            fs::create_dir_all(&adapter_dir).unwrap();
            fs::write(
                adapter_dir.join("index.json"),
                r#"{
                    "schema": "homeboy-extension/dependency-adapter-index/v1",
                    "manifests": [{ "id": "fixture", "ecosystem": "fixture", "path": "fixture.json" }]
                }"#,
            )
            .unwrap();
            fs::write(
                adapter_dir.join("fixture.json"),
                r#"{
                    "schema": "homeboy-extension/dependency-adapter-manifest/v1",
                    "id": "fixture",
                    "version": 1,
                    "ecosystem": "fixture",
                    "project_signals": { "root_files": ["fixture.json"] },
                    "capabilities": {},
                    "package_managers": [{
                        "id": "fixture-pm",
                        "selection": { "priority": 1, "default": true },
                        "install": { "intent": "install" },
                        "commands": {},
                        "outputs": [{ "kind": "directory", "path": "deps" }]
                    }]
                }"#,
            )
            .unwrap();

            let providers = resolve_dependency_providers_optional(&component, &project).unwrap();
            assert!(matches!(
                providers.as_slice(),
                [DependencyProvider::Adapter(_)]
            ));

            fs::write(
                project.join(DEPENDENCY_ADAPTER_MANIFEST),
                r#"{
                    "provider": "legacy-fixture",
                    "commands": { "install": { "argv": ["true"] } }
                }"#,
            )
            .unwrap();
            let providers = resolve_dependency_providers_optional(&component, &project).unwrap();
            assert!(matches!(
                providers.as_slice(),
                [DependencyProvider::Manifest(_)]
            ));
        });
    }

    #[test]
    fn registry_adapter_dispatches_declared_commands() {
        crate::test_support::with_isolated_home(|_| {
            let dir = tempdir().unwrap();
            let project = dir.path().join("project");
            fs::create_dir_all(&project).unwrap();
            fs::write(
                project.join("fixture.json"),
                r#"{"name":"fixture/project","dependencies":{"fixture/package":"^1.0"}}"#,
            )
            .unwrap();
            fs::write(
                project.join("adapter.sh"),
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> adapter.log\n",
            )
            .unwrap();
            make_executable(&project.join("adapter.sh"));

            let adapter_dir = paths::extensions()
                .unwrap()
                .join("fixture-extension/dependency-adapters");
            fs::create_dir_all(&adapter_dir).unwrap();
            fs::write(
                adapter_dir.join("index.json"),
                r#"{
                    "schema": "homeboy-extension/dependency-adapter-index/v1",
                    "manifests": [{ "id": "fixture", "ecosystem": "fixture", "path": "fixture.json" }]
                }"#,
            )
            .unwrap();
            fs::write(
                adapter_dir.join("fixture.json"),
                r#"{
                    "schema": "homeboy-extension/dependency-adapter-manifest/v1",
                    "id": "fixture",
                    "version": 1,
                    "ecosystem": "fixture",
                    "project_signals": { "root_files": ["fixture.json"] },
                    "capabilities": {},
                    "package_managers": [{
                        "id": "fixture-pm",
                        "selection": { "priority": 1, "default": true },
                        "install": { "intent": "install" },
                        "commands": {
                            "status": { "command": "sh adapter.sh status" },
                            "install": { "command": "sh adapter.sh install" },
                            "update": { "command": "sh adapter.sh update" }
                        },
                        "package_identity": {
                            "manifest": "fixture.json",
                            "name": "name",
                            "version": "version",
                            "dependencies": ["dependencies"]
                        },
                        "outputs": [{ "kind": "directory", "path": "deps" }]
                    }]
                }"#,
            )
            .unwrap();

            let component = Component::new(
                "fixture".to_string(),
                project.display().to_string(),
                String::new(),
                None,
            );
            let providers = resolve_dependency_providers_optional(&component, &project).unwrap();
            let DependencyProvider::Adapter(provider) = &providers[0] else {
                panic!("expected registry adapter");
            };
            let status = provider
                .status(DependencyProviderStatusRequest {
                    context: DependencyProviderContext {
                        component: &component,
                        path: &project,
                    },
                    package_filter: None,
                })
                .unwrap();
            assert_eq!(status.package_manager, "fixture-pm");
            assert_eq!(status.dependency_identities, vec!["fixture/project"]);
            assert_eq!(status.packages[0].name, "fixture/package");
            provider
                .install(DependencyProviderContext {
                    component: &component,
                    path: &project,
                })
                .unwrap();
            provider
                .update(DependencyProviderUpdateRequest {
                    context: DependencyProviderContext {
                        component: &component,
                        path: &project,
                    },
                    package: "fixture/package",
                    constraint: None,
                })
                .unwrap();
            assert_eq!(
                fs::read_to_string(project.join("adapter.log")).unwrap(),
                "status\ninstall\nstatus\nupdate\nstatus\n"
            );
        });
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = fs::metadata(path).unwrap().permissions();
            mode.set_mode(0o755);
            fs::set_permissions(path, mode).unwrap();
        }
    }
}
