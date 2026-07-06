use crate::core::component::Component;
use crate::core::deps::{DependencyCommandResult, DependencyPackage, DependencyUpdateResult};
use crate::core::extension::{self, ExtensionCapability, ExtensionExecutionContext};
use crate::core::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
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
    pub(crate) fn new(
        program: impl Into<String>,
        args: Vec<String>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
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
                "Declare a dependency provider with homeboy-deps.json, a component deps script, or an extension deps provider".to_string(),
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

    if let Some(provider) = ManifestDependencyProvider::load(path)? {
        providers.push(DependencyProvider::Manifest(provider));
        return Ok(providers);
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
            Ok(context) => providers.push(DependencyProvider::Extension(Box::new(
                ExtensionDependencyProvider { context },
            ))),
            Err(err) if providers.is_empty() => return Err(err),
            Err(_) => {}
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
    use crate::core::component::Component;
    use crate::core::extension::ExtensionCapability;
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
            context: crate::core::extension::ExtensionExecutionContext {
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
