use crate::core::component::Component;
use crate::core::deps::{DependencyCommandResult, DependencyPackage, DependencyUpdateResult};
use crate::core::extension::{self, ExtensionCapability, ExtensionExecutionContext};
use crate::core::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    Require { constraint: String },
    Update,
}

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

pub(crate) enum DependencyProvider {
    Composer(ComposerDependencyProvider),
    Extension(ExtensionDependencyProvider),
    ComponentScript(ComponentScriptDependencyProvider),
}

impl DependencyProvider {
    pub(crate) fn status(
        &self,
        component: &Component,
        path: &Path,
        package_filter: Option<&str>,
    ) -> Result<ProviderDependencyStatus> {
        match self {
            DependencyProvider::Composer(provider) => provider.status(path, package_filter),
            DependencyProvider::Extension(provider) => {
                provider.status(component, path, package_filter)
            }
            DependencyProvider::ComponentScript(provider) => {
                provider.status(component, path, package_filter)
            }
        }
    }

    pub(crate) fn handles_package(&self, path: &Path, package: &str) -> Result<bool> {
        match self {
            DependencyProvider::Composer(provider) => provider.handles_package(path, package),
            DependencyProvider::Extension(_) => Ok(true),
            DependencyProvider::ComponentScript(_) => Ok(true),
        }
    }

    pub(crate) fn update(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
        constraint: Option<&str>,
    ) -> Result<DependencyUpdateResult> {
        match self {
            DependencyProvider::Composer(provider) => {
                provider.update(component, path, package, constraint)
            }
            DependencyProvider::Extension(provider) => {
                provider.update(component, path, package, constraint)
            }
            DependencyProvider::ComponentScript(provider) => {
                provider.update(component, path, package, constraint)
            }
        }
    }

    pub(crate) fn install(
        &self,
        component: &Component,
        path: &Path,
    ) -> Result<Option<DependencyCommandResult>> {
        match self {
            DependencyProvider::Composer(provider) => provider.install(component, path),
            DependencyProvider::ComponentScript(provider) => provider.install(component, path),
            DependencyProvider::Extension(provider) => provider.install(component, path),
        }
    }
}

pub(crate) fn resolve_dependency_providers(
    component: &Component,
    path: &Path,
) -> Result<Vec<DependencyProvider>> {
    let mut providers = Vec::new();

    if ComposerDependencyProvider::supports(path) {
        providers.push(DependencyProvider::Composer(ComposerDependencyProvider));
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
        match extension::resolve_execution_context(component, ExtensionCapability::Deps) {
            Ok(context) => {
                providers.push(DependencyProvider::Extension(ExtensionDependencyProvider {
                    context,
                }))
            }
            Err(err) if providers.is_empty() => return Err(err),
            Err(_) => {}
        }
    }

    if providers.is_empty() {
        return Err(Error::validation_invalid_argument(
            "dependency_provider",
            format!("No dependency provider found for {}", path.display()),
            None,
            Some(vec![
                "Link an extension with deps support, or use a component with a supported dependency provider".to_string(),
                "Package managers are resolved through dependency providers, not core command orchestration".to_string(),
            ]),
        ));
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

pub fn composer_command_args(package: &str, action: &ComposerAction) -> Vec<String> {
    match action {
        ComposerAction::Require { constraint } => vec![
            "require".to_string(),
            format!("{package}:{constraint}"),
            "--with-dependencies".to_string(),
            "--no-interaction".to_string(),
        ],
        ComposerAction::Update => vec![
            "update".to_string(),
            package.to_string(),
            "--with-dependencies".to_string(),
            "--no-interaction".to_string(),
        ],
    }
}

pub(crate) struct ComposerDependencyProvider;

impl ComposerDependencyProvider {
    fn supports(path: &Path) -> bool {
        path.join("composer.json").is_file()
    }

    fn status(
        &self,
        path: &Path,
        package_filter: Option<&str>,
    ) -> Result<ProviderDependencyStatus> {
        Ok(ProviderDependencyStatus {
            package_manager: "composer".to_string(),
            dependency_identities: composer_identities(path)?,
            packages: read_composer_packages(path, package_filter)?,
        })
    }

    fn handles_package(&self, path: &Path, package: &str) -> Result<bool> {
        Ok(package_snapshot(path, package)?.is_some())
    }

    fn update(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
        constraint: Option<&str>,
    ) -> Result<DependencyUpdateResult> {
        let before = package_snapshot(path, package)?;
        let action = match constraint {
            Some(constraint) => ComposerAction::Require {
                constraint: constraint.to_string(),
            },
            None => ComposerAction::Update,
        };
        let args = composer_command_args(package, &action);
        let output = Command::new("composer")
            .args(&args)
            .current_dir(path)
            .output()
            .map_err(|e| {
                Error::internal_io(e.to_string(), Some("run dependency provider".to_string()))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                format!(
                    "Dependency provider command failed with status {}: {}",
                    output.status,
                    first_non_empty_line(&stderr)
                        .or_else(|| first_non_empty_line(&stdout))
                        .unwrap_or("no output")
                ),
                None,
                Some(vec![format!(
                    "Run manually in {}: composer {}",
                    path.display(),
                    args.join(" ")
                )]),
            ));
        }

        let after = package_snapshot(path, package)?;

        Ok(DependencyUpdateResult {
            component_id: component.id.clone(),
            component_path: path.display().to_string(),
            package_manager: "composer".to_string(),
            package: package.to_string(),
            requested_constraint: constraint.map(str::to_string),
            command: std::iter::once("composer".to_string())
                .chain(args)
                .collect(),
            before,
            after,
            stdout,
            stderr,
            install: None,
            rebuild: None,
        })
    }

    fn install(
        &self,
        _component: &Component,
        _path: &Path,
    ) -> Result<Option<DependencyCommandResult>> {
        Ok(None)
    }
}

pub(crate) struct ComponentScriptDependencyProvider;

impl ComponentScriptDependencyProvider {
    fn status(
        &self,
        component: &Component,
        path: &Path,
        package_filter: Option<&str>,
    ) -> Result<ProviderDependencyStatus> {
        let mut args = vec!["status".to_string()];
        if let Some(package_filter) = package_filter {
            args.push(package_filter.to_string());
        }
        let output = run_component_deps_script(component, path, &args)?;
        let status: ExtensionStatusOutput = parse_extension_output(&output.stdout, "deps status")?;

        Ok(ProviderDependencyStatus {
            package_manager: status.package_manager,
            dependency_identities: status.dependency_identities,
            packages: status.packages,
        })
    }

    fn update(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
        constraint: Option<&str>,
    ) -> Result<DependencyUpdateResult> {
        let mut args = vec!["update".to_string(), package.to_string()];
        if let Some(constraint) = constraint {
            args.push(constraint.to_string());
        }
        let output = run_component_deps_script(component, path, &args)?;
        let mut result: DependencyUpdateResult =
            parse_extension_output(&output.stdout, "deps update")?;
        result.component_id = component.id.clone();
        result.component_path = path.display().to_string();
        result.package = package.to_string();
        result.requested_constraint = constraint.map(str::to_string);
        result.stdout = output.stdout;
        result.stderr = output.stderr;
        result.install = None;
        result.rebuild = None;
        Ok(result)
    }

    fn install(
        &self,
        component: &Component,
        path: &Path,
    ) -> Result<Option<DependencyCommandResult>> {
        let args = vec!["install".to_string()];
        let output = run_component_deps_script(component, path, &args)?;
        Ok(Some(DependencyCommandResult {
            command: component_deps_script_command(component, &args),
            skipped: false,
            status: Some(output.exit_code),
            stdout: output.stdout,
            stderr: output.stderr,
        }))
    }
}

pub(crate) struct ExtensionDependencyProvider {
    context: ExtensionExecutionContext,
}

impl ExtensionDependencyProvider {
    fn status(
        &self,
        component: &Component,
        path: &Path,
        package_filter: Option<&str>,
    ) -> Result<ProviderDependencyStatus> {
        let mut args = vec!["status".to_string()];
        if let Some(package_filter) = package_filter {
            args.push(package_filter.to_string());
        }
        let output = self.run(component, path, &args)?;
        let status: ExtensionStatusOutput = parse_extension_output(&output.stdout, "deps status")?;

        Ok(ProviderDependencyStatus {
            package_manager: status.package_manager,
            dependency_identities: status.dependency_identities,
            packages: status.packages,
        })
    }

    fn update(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
        constraint: Option<&str>,
    ) -> Result<DependencyUpdateResult> {
        let mut args = vec!["update".to_string(), package.to_string()];
        if let Some(constraint) = constraint {
            args.push(constraint.to_string());
        }
        let output = self.run(component, path, &args)?;
        let mut result: DependencyUpdateResult =
            parse_extension_output(&output.stdout, "deps update")?;
        result.component_id = component.id.clone();
        result.component_path = path.display().to_string();
        result.package = package.to_string();
        result.requested_constraint = constraint.map(str::to_string);
        result.stdout = output.stdout;
        result.stderr = output.stderr;
        result.install = None;
        result.rebuild = None;
        Ok(result)
    }

    fn install(
        &self,
        component: &Component,
        path: &Path,
    ) -> Result<Option<DependencyCommandResult>> {
        let args = vec!["install".to_string()];
        let output = self.run(component, path, &args)?;
        Ok(Some(DependencyCommandResult {
            command: extension_deps_command(&args),
            skipped: false,
            status: Some(output.exit_code),
            stdout: output.stdout,
            stderr: output.stderr,
        }))
    }

    fn run(
        &self,
        component: &Component,
        path: &Path,
        args: &[String],
    ) -> Result<crate::core::extension::RunnerOutput> {
        crate::core::extension::ExtensionRunner::for_context(self.context.clone())
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
) -> Result<crate::core::extension::component_script::ComponentScriptOutput> {
    let output = crate::core::extension::component_script::run_component_scripts_with_env(
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

#[derive(Debug, Deserialize)]
struct ExtensionStatusOutput {
    package_manager: String,
    #[serde(default)]
    dependency_identities: Vec<String>,
    #[serde(default)]
    packages: Vec<DependencyPackage>,
}

fn parse_extension_output<T: for<'de> Deserialize<'de>>(stdout: &str, action: &str) -> Result<T> {
    serde_json::from_str(stdout).map_err(|e| {
        Error::validation_invalid_json(e, Some(action.to_string()), Some(stdout.to_string()))
    })
}

fn package_snapshot(path: &Path, package: &str) -> Result<Option<DependencyPackage>> {
    Ok(read_composer_packages(path, Some(package))?
        .into_iter()
        .next())
}

fn composer_identities(path: &Path) -> Result<Vec<String>> {
    let manifest = read_json_file(&path.join("composer.json"))?;
    Ok(manifest
        .get("name")
        .and_then(Value::as_str)
        .map(|name| vec![name.to_string()])
        .unwrap_or_default())
}

fn read_composer_packages(
    path: &Path,
    package_filter: Option<&str>,
) -> Result<Vec<DependencyPackage>> {
    let manifest = read_json_file(&path.join("composer.json"))?;
    let lock = read_optional_json_file(&path.join("composer.lock"))?;
    let mut direct = BTreeMap::new();

    collect_manifest_section(&manifest, "require", &mut direct);
    collect_manifest_section(&manifest, "require-dev", &mut direct);

    let locked = lock
        .as_ref()
        .map(collect_locked_packages)
        .unwrap_or_default();

    let mut names: BTreeSet<String> = direct.keys().cloned().collect();
    names.extend(locked.keys().cloned());

    let packages = names
        .into_iter()
        .filter(|name| package_filter.map(|filter| filter == name).unwrap_or(true))
        .map(|name| {
            let (manifest_section, constraint) = direct
                .get(&name)
                .cloned()
                .map(|(section, constraint)| (Some(section), Some(constraint)))
                .unwrap_or((None, None));
            let locked = locked.get(&name);
            DependencyPackage {
                name,
                manifest_section,
                constraint,
                locked_version: locked.and_then(|p| p.version.clone()),
                locked_reference: locked.and_then(|p| p.reference.clone()),
            }
        })
        .collect();

    Ok(packages)
}

fn collect_manifest_section(
    manifest: &Value,
    section: &str,
    direct: &mut BTreeMap<String, (String, String)>,
) {
    let Some(entries) = manifest.get(section).and_then(Value::as_object) else {
        return;
    };

    for (name, constraint) in entries {
        if name == "php" || name.starts_with("ext-") {
            continue;
        }
        if let Some(constraint) = constraint.as_str() {
            direct.insert(name.clone(), (section.to_string(), constraint.to_string()));
        }
    }
}

#[derive(Debug, Clone, Default)]
struct LockedPackage {
    version: Option<String>,
    reference: Option<String>,
}

fn collect_locked_packages(lock: &Value) -> BTreeMap<String, LockedPackage> {
    let mut packages = BTreeMap::new();

    for section in ["packages", "packages-dev"] {
        let Some(entries) = lock.get(section).and_then(Value::as_array) else {
            continue;
        };

        for entry in entries {
            let Some(name) = entry.get("name").and_then(Value::as_str) else {
                continue;
            };
            let version = entry
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string);
            let reference = entry
                .get("source")
                .and_then(|source| source.get("reference"))
                .or_else(|| entry.get("dist").and_then(|dist| dist.get("reference")))
                .and_then(Value::as_str)
                .map(str::to_string);

            packages.insert(name.to_string(), LockedPackage { version, reference });
        }
    }

    packages
}

fn read_json_file(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw)))
}

fn read_optional_json_file(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    read_json_file(path).map(Some)
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().find(|line| !line.trim().is_empty())
}
