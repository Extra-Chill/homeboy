use crate::config::{
    AppConfig, AppPaths, ComponentConfiguration, ProjectConfiguration, ServerConfig,
};
use crate::json::{read_json_file, write_json_file_pretty};
use crate::module::ModuleManifest;
use serde::Serialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::module_settings::ModuleSettingsValidator;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorSeverity {
    Info,
    Warning,
    Error,
}

impl DoctorSeverity {
    fn sort_key(&self) -> u8 {
        match self {
            DoctorSeverity::Error => 0,
            DoctorSeverity::Warning => 1,
            DoctorSeverity::Info => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorIssue {
    pub severity: DoctorSeverity,
    pub code: String,
    pub message: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSummary {
    pub files_scanned: usize,
    pub issues: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub command: String,
    pub summary: DoctorSummary,
    pub issues: Vec<DoctorIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorScope {
    All,
    App,
    Projects,
    Servers,
    Components,
    Modules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOn {
    Error,
    Warning,
}

pub struct Doctor;

impl Doctor {
    pub fn scan(scope: DoctorScope) -> crate::Result<DoctorScanResult> {
        let mut scanner = Scanner::new("doctor.scan");
        scanner.scan(scope);
        Ok(scanner.finish())
    }

    pub fn scan_file(path: &Path) -> crate::Result<DoctorScanResult> {
        let mut scanner = Scanner::new("doctor.scan");
        scanner.scan_file(path);
        Ok(scanner.finish())
    }

    pub fn cleanup(scope: DoctorScope, dry_run: bool) -> crate::Result<DoctorCleanupAndScan> {
        let cleanup_result = Cleaner::cleanup_scope(scope, dry_run)?;
        let scan_result = Doctor::scan(scope)?;

        Ok(DoctorCleanupAndScan {
            cleanup: cleanup_result,
            scan: scan_result.report,
        })
    }

    pub fn cleanup_file(path: &Path, dry_run: bool) -> crate::Result<DoctorCleanupAndScan> {
        let cleanup_result = Cleaner::cleanup_file(path, dry_run)?;
        let scan_result = Doctor::scan_file(path)?;

        Ok(DoctorCleanupAndScan {
            cleanup: cleanup_result,
            scan: scan_result.report,
        })
    }

    pub fn exit_code(result: &DoctorScanResult, fail_on: FailOn) -> i32 {
        Doctor::exit_code_from_report(&result.report, fail_on)
    }

    pub fn exit_code_from_report(report: &DoctorReport, fail_on: FailOn) -> i32 {
        let has_errors = report
            .issues
            .iter()
            .any(|i| i.severity == DoctorSeverity::Error);
        if has_errors {
            return 1;
        }

        if fail_on == FailOn::Warning {
            let has_warnings = report
                .issues
                .iter()
                .any(|i| i.severity == DoctorSeverity::Warning);
            if has_warnings {
                return 1;
            }
        }

        0
    }
}

pub struct DoctorScanResult {
    pub report: DoctorReport,
    pub files_scanned: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCleanupChange {
    pub file: String,
    pub schema: String,
    pub removed_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCleanupSkipped {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCleanupSummary {
    pub files_considered: usize,
    pub files_changed: usize,
    pub keys_removed: usize,
    pub files_skipped: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCleanupReport {
    pub command: String,
    pub summary: DoctorCleanupSummary,
    pub changes: Vec<DoctorCleanupChange>,
    pub skipped: Vec<DoctorCleanupSkipped>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCleanupAndScan {
    pub cleanup: DoctorCleanupReport,
    pub scan: DoctorReport,
}

struct Scanner {
    command: String,
    issues: Vec<DoctorIssue>,
    files_scanned: Vec<String>,
    app_config: Option<AppConfig>,
    projects: BTreeMap<String, ProjectConfiguration>,
    servers: BTreeMap<String, ServerConfig>,
    components: BTreeMap<String, ComponentConfiguration>,
    modules: BTreeMap<String, ModuleManifest>,
}

impl Scanner {
    fn new(command: &str) -> Self {
        Self {
            command: command.to_string(),
            issues: Vec::new(),
            files_scanned: Vec::new(),
            app_config: None,
            projects: BTreeMap::new(),
            servers: BTreeMap::new(),
            components: BTreeMap::new(),
            modules: BTreeMap::new(),
        }
    }

    fn scan(&mut self, scope: DoctorScope) {
        match scope {
            DoctorScope::All => {
                self.scan(DoctorScope::App);
                self.scan(DoctorScope::Projects);
                self.scan(DoctorScope::Servers);
                self.scan(DoctorScope::Components);
                self.scan(DoctorScope::Modules);
                self.validate_cross_refs();
            }
            DoctorScope::App => {
                let Ok(path) = AppPaths::config() else {
                    return;
                };
                if path.exists() {
                    self.scan_app_config(&path);
                }
            }
            DoctorScope::Projects => {
                let Ok(dir) = AppPaths::projects() else {
                    return;
                };
                self.scan_dir_json(dir, FileKind::Project)
            }
            DoctorScope::Servers => {
                let Ok(dir) = AppPaths::servers() else {
                    return;
                };
                self.scan_dir_json(dir, FileKind::Server)
            }
            DoctorScope::Components => {
                let Ok(dir) = AppPaths::components() else {
                    return;
                };
                self.scan_dir_json(dir, FileKind::Component)
            }
            DoctorScope::Modules => self.scan_modules(),
        }
    }

    fn scan_file(&mut self, path: &Path) {
        if let Some(kind) = classify_file(path) {
            match kind {
                FileKind::App => self.scan_app_config(path),
                FileKind::Project => {
                    let path_buf = path.to_path_buf();
                    if let Some((raw, typed)) = self.read_typed_json_file::<ProjectConfiguration>(
                        &path_buf,
                        "ProjectConfiguration",
                    ) {
                        self.on_project_file(&path_buf, raw, typed);
                    }
                }
                FileKind::Server => {
                    let path_buf = path.to_path_buf();
                    if let Some((raw, typed)) =
                        self.read_typed_json_file::<ServerConfig>(&path_buf, "ServerConfig")
                    {
                        self.on_server_file(&path_buf, raw, typed);
                    }
                }
                FileKind::Component => {
                    let path_buf = path.to_path_buf();
                    if let Some((raw, typed)) = self.read_typed_json_file::<ComponentConfiguration>(
                        &path_buf,
                        "ComponentConfiguration",
                    ) {
                        self.on_component_file(&path_buf, raw, typed);
                    }
                }
                FileKind::ModuleManifest => {
                    let path_buf = path.to_path_buf();
                    if let Some((raw, typed)) =
                        self.read_typed_json_file::<ModuleManifest>(&path_buf, "ModuleManifest")
                    {
                        self.on_module_manifest_file(&path_buf, raw, typed);
                    }
                }
            }
            self.validate_cross_refs();
            return;
        }

        self.scan_generic_json(path);
    }

    fn scan_app_config(&mut self, path: &Path) {
        let path_buf = path.to_path_buf();
        if let Some((raw, typed)) = self.read_typed_json_file::<AppConfig>(&path_buf, "AppConfig") {
            self.emit_unknown_keys(&path_buf, "AppConfig", &raw, &typed);
            self.emit_app_installed_module_settings_issues(&path_buf, raw.get("installedModules"));
            self.app_config = Some(typed);
        }
    }

    fn scan_modules(&mut self) {
        let Ok(modules_dir) = AppPaths::modules() else {
            return;
        };
        if !modules_dir.exists() {
            return;
        }

        let Ok(entries) = fs::read_dir(&modules_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("homeboy.json");
            if !manifest_path.exists() {
                continue;
            }

            if let Some((raw, typed)) =
                self.read_typed_json_file::<ModuleManifest>(&manifest_path, "ModuleManifest")
            {
                self.on_module_manifest_file(&manifest_path, raw, typed);
            }
        }
    }

    fn scan_dir_json(&mut self, dir: PathBuf, kind: FileKind) {
        if !dir.exists() {
            return;
        }

        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }

            match kind {
                FileKind::Project => {
                    if let Some((raw, typed)) = self
                        .read_typed_json_file::<ProjectConfiguration>(&path, "ProjectConfiguration")
                    {
                        self.on_project_file(&path, raw, typed);
                    }
                }
                FileKind::Server => {
                    if let Some((raw, typed)) =
                        self.read_typed_json_file::<ServerConfig>(&path, "ServerConfig")
                    {
                        self.on_server_file(&path, raw, typed);
                    }
                }
                FileKind::Component => {
                    if let Some((raw, typed)) = self.read_typed_json_file::<ComponentConfiguration>(
                        &path,
                        "ComponentConfiguration",
                    ) {
                        self.on_component_file(&path, raw, typed);
                    }
                }
                _ => {}
            }
        }
    }

    fn scan_generic_json(&mut self, path: &Path) {
        self.track_file(path);
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(err) => {
                self.push_issue(
                    DoctorSeverity::Error,
                    "IO_READ_ERROR",
                    "Failed to read file",
                    path,
                    None,
                    Some(serde_json::json!({"error": err.to_string()})),
                );
                return;
            }
        };

        if let Err(err) = serde_json::from_str::<Value>(&content) {
            self.push_issue(
                DoctorSeverity::Error,
                "JSON_PARSE_ERROR",
                "Malformed JSON",
                path,
                None,
                Some(serde_json::json!({"error": err.to_string()})),
            );
        }
    }

    fn read_typed_json_file<T>(&mut self, path: &Path, expected: &str) -> Option<(Value, T)>
    where
        T: serde::de::DeserializeOwned,
    {
        self.track_file(path);

        let raw: Value = match read_json_file(path) {
            Ok(v) => v,
            Err(err) => {
                self.push_issue(
                    DoctorSeverity::Error,
                    "JSON_READ_ERROR",
                    &err.to_string(),
                    path,
                    None,
                    None,
                );
                return None;
            }
        };

        let typed: T = match serde_json::from_value(raw.clone()) {
            Ok(v) => v,
            Err(err) => {
                self.push_issue(
                    DoctorSeverity::Error,
                    "SCHEMA_DESERIALIZE_ERROR",
                    &format!("JSON does not match expected schema: {expected}"),
                    path,
                    None,
                    Some(serde_json::json!({"error": err.to_string()})),
                );
                return None;
            }
        };

        Some((raw, typed))
    }

    fn on_project_file(&mut self, path: &Path, raw: Value, project: ProjectConfiguration) {
        self.emit_unknown_keys(path, "ProjectConfiguration", &raw, &project);
        self.emit_module_settings_issues(path, "project", raw.get("modules"));
        let id = file_stem_id(path);

        if project.name.trim().is_empty() {
            self.push_issue(
                DoctorSeverity::Error,
                "INVALID_VALUE",
                "Project name is empty",
                path,
                Some("/name".to_string()),
                None,
            );
        }
        if project.domain.trim().is_empty() {
            self.push_issue(
                DoctorSeverity::Error,
                "INVALID_VALUE",
                "Project domain is empty",
                path,
                Some("/domain".to_string()),
                None,
            );
        }
        // modules array can be empty - project is just a generic SSH target

        self.projects.insert(id, project);
    }

    fn on_server_file(&mut self, path: &Path, raw: Value, server: ServerConfig) {
        self.emit_unknown_keys(path, "ServerConfig", &raw, &server);
        if !server.is_valid() {
            self.push_issue(
                DoctorSeverity::Error,
                "INVALID_VALUE",
                "Server must have non-empty host and user",
                path,
                None,
                Some(serde_json::json!({"host": server.host, "user": server.user})),
            );
        }

        let id = file_stem_id(path);
        self.servers.insert(id, server);
    }

    fn on_component_file(&mut self, path: &Path, raw: Value, component: ComponentConfiguration) {
        self.emit_unknown_keys(path, "ComponentConfiguration", &raw, &component);
        self.emit_module_settings_issues(path, "component", raw.get("modules"));
        let id = file_stem_id(path);
        self.components.insert(id, component);
    }

    fn on_module_manifest_file(&mut self, path: &Path, raw: Value, mut manifest: ModuleManifest) {
        self.emit_unknown_keys(path, "ModuleManifest", &raw, &manifest);
        manifest.module_path = Some(
            path.parent()
                .unwrap_or_else(|| Path::new(""))
                .to_string_lossy()
                .to_string(),
        );

        let missing = [
            ("id", manifest.id.trim().is_empty()),
            ("name", manifest.name.trim().is_empty()),
            ("version", manifest.version.trim().is_empty()),
            ("icon", manifest.icon.trim().is_empty()),
            (
                "description",
                manifest
                    .description
                    .as_ref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true),
            ),
            (
                "author",
                manifest
                    .author
                    .as_ref()
                    .map(|s| s.trim().is_empty())
                    .unwrap_or(true),
            ),
        ]
        .into_iter()
        .filter_map(|(field, is_bad)| if is_bad { Some(field) } else { None })
        .collect::<Vec<_>>();

        if !missing.is_empty() {
            self.push_issue(
                DoctorSeverity::Error,
                "INVALID_VALUE",
                "Module manifest has empty required fields",
                path,
                None,
                Some(serde_json::json!({"fields": missing})),
            );
        }

        self.modules.insert(manifest.id.clone(), manifest);
    }

    fn unknown_top_level_keys<T: serde::Serialize>(
        &mut self,
        path: &Path,
        schema: &str,
        raw: &Value,
        typed: &T,
    ) -> Vec<String> {
        let Some(raw_obj) = raw.as_object() else {
            self.push_issue(
                DoctorSeverity::Error,
                "SCHEMA_TYPE_ERROR",
                &format!("Expected JSON object for {schema}"),
                path,
                None,
                None,
            );
            return Vec::new();
        };

        let typed_value = match serde_json::to_value(typed) {
            Ok(v) => v,
            Err(err) => {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INTERNAL_SERIALIZE_ERROR",
                    "Failed to serialize typed config for comparison",
                    path,
                    None,
                    Some(serde_json::json!({"error": err.to_string()})),
                );
                return Vec::new();
            }
        };

        let Some(typed_obj) = typed_value.as_object() else {
            return Vec::new();
        };

        let raw_keys: BTreeSet<&String> = raw_obj.keys().collect();
        let typed_keys: BTreeSet<&String> = typed_obj.keys().collect();

        raw_keys
            .difference(&typed_keys)
            .map(|s| (*s).clone())
            .collect()
    }

    fn emit_unknown_keys<T: serde::Serialize>(
        &mut self,
        path: &Path,
        schema: &str,
        raw: &Value,
        typed: &T,
    ) {
        let unknown = self.unknown_top_level_keys(path, schema, raw, typed);

        if !unknown.is_empty() {
            self.push_issue(
                DoctorSeverity::Warning,
                "UNKNOWN_KEYS",
                "File contains keys not in current schema",
                path,
                None,
                Some(serde_json::json!({"schema": schema, "keys": unknown})),
            );
        }
    }

    fn emit_app_installed_module_settings_issues(
        &mut self,
        path: &Path,
        raw_installed: Option<&Value>,
    ) {
        let Some(raw_installed) = raw_installed else {
            return;
        };

        let Some(installed_obj) = raw_installed.as_object() else {
            self.push_issue(
                DoctorSeverity::Error,
                "INVALID_VALUE",
                "installedModules must be an object",
                path,
                Some("/installedModules".to_string()),
                None,
            );
            return;
        };

        for (module_id, config) in installed_obj {
            let validator = if let Some(manifest) = self.modules.get(module_id) {
                ModuleSettingsValidator::new(manifest)
            } else {
                self.push_issue(
                    DoctorSeverity::Error,
                    "BROKEN_REFERENCE",
                    "installedModules references missing module manifest",
                    path,
                    Some(format!("/installedModules/{module_id}")),
                    Some(serde_json::json!({"id": module_id})),
                );
                continue;
            };

            let Some(config_obj) = config.as_object() else {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INVALID_VALUE",
                    "installedModules entry must be an object",
                    path,
                    Some(format!("/installedModules/{module_id}")),
                    None,
                );
                continue;
            };

            let Some(settings_val) = config_obj.get("settings") else {
                continue;
            };

            let Some(settings_obj) = settings_val.as_object() else {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INVALID_VALUE",
                    "installedModules.settings must be an object",
                    path,
                    Some(format!("/installedModules/{module_id}/settings")),
                    None,
                );
                continue;
            };

            if let Err(err) = validator.validate_json_object("app", settings_obj) {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INVALID_VALUE",
                    &err.to_string(),
                    path,
                    Some(format!("/installedModules/{module_id}/settings")),
                    Some(serde_json::json!({
                        "scope": "app",
                        "moduleId": module_id,
                    })),
                );
            }
        }
    }

    fn emit_module_settings_issues(
        &mut self,
        path: &Path,
        scope: &str,
        raw_modules: Option<&Value>,
    ) {
        let Some(raw_modules) = raw_modules else {
            return;
        };

        let Some(modules_obj) = raw_modules.as_object() else {
            self.push_issue(
                DoctorSeverity::Error,
                "INVALID_VALUE",
                &format!("{scope} modules must be an object"),
                path,
                Some("/modules".to_string()),
                None,
            );
            return;
        };

        for (module_id, module_config) in modules_obj {
            let validator = if let Some(manifest) = self.modules.get(module_id) {
                ModuleSettingsValidator::new(manifest)
            } else {
                self.push_issue(
                    DoctorSeverity::Error,
                    "BROKEN_REFERENCE",
                    &format!("{scope} references missing module manifest"),
                    path,
                    Some(format!("/modules/{module_id}")),
                    Some(serde_json::json!({"id": module_id})),
                );
                continue;
            };

            let Some(config_obj) = module_config.as_object() else {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INVALID_VALUE",
                    &format!("{scope} module config must be an object"),
                    path,
                    Some(format!("/modules/{module_id}")),
                    None,
                );
                continue;
            };

            let settings_val = config_obj.get("settings");
            let Some(settings_val) = settings_val else {
                continue;
            };

            let Some(settings_obj) = settings_val.as_object() else {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INVALID_VALUE",
                    &format!("{scope} module settings must be an object"),
                    path,
                    Some(format!("/modules/{module_id}/settings")),
                    None,
                );
                continue;
            };

            if let Err(err) = validator.validate_json_object(scope, settings_obj) {
                self.push_issue(
                    DoctorSeverity::Error,
                    "INVALID_VALUE",
                    &err.to_string(),
                    path,
                    Some(format!("/modules/{module_id}/settings")),
                    Some(serde_json::json!({
                        "scope": scope,
                        "moduleId": module_id,
                    })),
                );
            }
        }
    }

    fn validate_cross_refs(&mut self) {
        let mut extra_issues = Vec::new();

        for (project_id, project) in &self.projects {
            if let Some(server_id) = project.server_id.as_deref() {
                if !self.servers.contains_key(server_id) {
                    extra_issues.push(DoctorIssue {
                        severity: DoctorSeverity::Warning,
                        code: "BROKEN_REFERENCE".to_string(),
                        message: "project.serverId references missing server".to_string(),
                        file: AppPaths::project(project_id)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| "<unresolved project path>".to_string()),
                        pointer: Some("/serverId".to_string()),
                        details: Some(serde_json::json!({"id": server_id})),
                    });
                }
            }

            for component_id in &project.component_ids {
                if !self.components.contains_key(component_id) {
                    extra_issues.push(DoctorIssue {
                        severity: DoctorSeverity::Warning,
                        code: "BROKEN_REFERENCE".to_string(),
                        message: "project.componentIds contains missing component".to_string(),
                        file: AppPaths::project(project_id)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| "<unresolved project path>".to_string()),
                        pointer: Some("/componentIds".to_string()),
                        details: Some(serde_json::json!({"id": component_id})),
                    });
                }
            }
        }

        for (module_id, manifest) in &self.modules {
            let Some(requires) = &manifest.requires else {
                continue;
            };
            if requires.components.is_empty() {
                continue;
            }

            for component_id in &requires.components {
                if self.components.contains_key(component_id) {
                    continue;
                }
                let module_dir = manifest.module_path.as_deref().unwrap_or(module_id);
                extra_issues.push(DoctorIssue {
                    severity: DoctorSeverity::Warning,
                    code: "BROKEN_REFERENCE".to_string(),
                    message: "module requires missing component".to_string(),
                    file: PathBuf::from(module_dir)
                        .join("homeboy.json")
                        .to_string_lossy()
                        .to_string(),
                    pointer: Some("/requires/components".to_string()),
                    details: Some(serde_json::json!({"id": component_id})),
                });
            }
        }

        self.issues.extend(extra_issues);
    }

    fn track_file(&mut self, path: &Path) {
        self.files_scanned.push(path.to_string_lossy().to_string());
    }

    fn push_issue(
        &mut self,
        severity: DoctorSeverity,
        code: &str,
        message: &str,
        file: &Path,
        pointer: Option<String>,
        details: Option<Value>,
    ) {
        self.issues.push(DoctorIssue {
            severity,
            code: code.to_string(),
            message: message.to_string(),
            file: file.to_string_lossy().to_string(),
            pointer,
            details,
        });
    }

    fn finish(mut self) -> DoctorScanResult {
        let mut counts = BTreeMap::new();
        counts.insert(
            "error".to_string(),
            self.issues
                .iter()
                .filter(|i| i.severity == DoctorSeverity::Error)
                .count(),
        );
        counts.insert(
            "warning".to_string(),
            self.issues
                .iter()
                .filter(|i| i.severity == DoctorSeverity::Warning)
                .count(),
        );
        counts.insert(
            "info".to_string(),
            self.issues
                .iter()
                .filter(|i| i.severity == DoctorSeverity::Info)
                .count(),
        );

        self.issues.sort_by(|a, b| {
            let by_severity = a.severity.sort_key().cmp(&b.severity.sort_key());
            if by_severity != Ordering::Equal {
                return by_severity;
            }
            let by_code = a.code.cmp(&b.code);
            if by_code != Ordering::Equal {
                return by_code;
            }
            a.file.cmp(&b.file)
        });

        DoctorScanResult {
            report: DoctorReport {
                command: self.command.clone(),
                summary: DoctorSummary {
                    files_scanned: self.files_scanned.len(),
                    issues: counts,
                },
                issues: self.issues,
            },
            files_scanned: self.files_scanned,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    App,
    Project,
    Server,
    Component,
    ModuleManifest,
}

fn classify_file(path: &Path) -> Option<FileKind> {
    if path.file_name().is_some_and(|n| n == "config.json") {
        return Some(FileKind::App);
    }

    let parent = path.parent().and_then(|p| p.file_name())?;

    match parent.to_string_lossy().as_ref() {
        "projects" => Some(FileKind::Project),
        "servers" => Some(FileKind::Server),
        "components" => Some(FileKind::Component),
        _ => {
            if path.file_name().is_some_and(|n| n == "homeboy.json") {
                Some(FileKind::ModuleManifest)
            } else {
                None
            }
        }
    }
}

fn file_stem_id(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

struct Cleaner;

impl Cleaner {
    fn cleanup_scope(scope: DoctorScope, dry_run: bool) -> crate::Result<DoctorCleanupReport> {
        let mut cleaner = CleanerState::new(dry_run);
        cleaner.cleanup_scope(scope)?;
        Ok(cleaner.finish())
    }

    fn cleanup_file(path: &Path, dry_run: bool) -> crate::Result<DoctorCleanupReport> {
        let mut cleaner = CleanerState::new(dry_run);
        cleaner.cleanup_file(path)?;
        Ok(cleaner.finish())
    }
}

struct CleanerState {
    dry_run: bool,
    changes: Vec<DoctorCleanupChange>,
    skipped: Vec<DoctorCleanupSkipped>,
    files_considered: usize,
}

impl CleanerState {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            changes: Vec::new(),
            skipped: Vec::new(),
            files_considered: 0,
        }
    }

    fn finish(self) -> DoctorCleanupReport {
        let files_changed = self.changes.len();
        let keys_removed: usize = self.changes.iter().map(|c| c.removed_keys.len()).sum();
        let files_skipped = self.skipped.len();

        DoctorCleanupReport {
            command: "doctor.cleanup".to_string(),
            summary: DoctorCleanupSummary {
                files_considered: self.files_considered,
                files_changed,
                keys_removed,
                files_skipped,
                dry_run: self.dry_run,
            },
            changes: self.changes,
            skipped: self.skipped,
        }
    }

    fn cleanup_scope(&mut self, scope: DoctorScope) -> crate::Result<()> {
        match scope {
            DoctorScope::All => {
                self.cleanup_scope(DoctorScope::App)?;
                self.cleanup_scope(DoctorScope::Projects)?;
                self.cleanup_scope(DoctorScope::Servers)?;
                self.cleanup_scope(DoctorScope::Components)?;
                self.cleanup_scope(DoctorScope::Modules)?;
            }
            DoctorScope::App => {
                let path = AppPaths::config()?;
                if path.exists() {
                    self.cleanup_typed_file::<AppConfig>(&path, "AppConfig")?;
                }
            }
            DoctorScope::Projects => {
                let dir = AppPaths::projects()?;
                self.cleanup_dir_json::<ProjectConfiguration>(&dir, "ProjectConfiguration")?;
            }
            DoctorScope::Servers => {
                let dir = AppPaths::servers()?;
                self.cleanup_dir_json::<ServerConfig>(&dir, "ServerConfig")?;
            }
            DoctorScope::Components => {
                let dir = AppPaths::components()?;
                self.cleanup_dir_json::<ComponentConfiguration>(&dir, "ComponentConfiguration")?;
            }
            DoctorScope::Modules => {
                let modules_dir = AppPaths::modules()?;
                if !modules_dir.exists() {
                    return Ok(());
                }

                let Ok(entries) = fs::read_dir(&modules_dir) else {
                    return Ok(());
                };

                for entry in entries.flatten() {
                    let module_dir = entry.path();
                    if !module_dir.is_dir() {
                        continue;
                    }
                    let manifest_path = module_dir.join("homeboy.json");
                    if !manifest_path.exists() {
                        continue;
                    }
                    self.cleanup_typed_file::<ModuleManifest>(&manifest_path, "ModuleManifest")?;
                }
            }
        }

        Ok(())
    }

    fn cleanup_file(&mut self, path: &Path) -> crate::Result<()> {
        let Some(kind) = classify_file(path) else {
            return Err(crate::Error::validation_invalid_argument(
                "file",
                "Path is not a recognized Homeboy config JSON file kind",
                None,
                Some(vec![
                    "config.json".to_string(),
                    "projects/*.json".to_string(),
                    "servers/*.json".to_string(),
                    "components/*.json".to_string(),
                    "modules/*/homeboy.json".to_string(),
                ]),
            ));
        };

        match kind {
            FileKind::App => self.cleanup_typed_file::<AppConfig>(path, "AppConfig"),
            FileKind::Project => {
                self.cleanup_typed_file::<ProjectConfiguration>(path, "ProjectConfiguration")
            }
            FileKind::Server => self.cleanup_typed_file::<ServerConfig>(path, "ServerConfig"),
            FileKind::Component => {
                self.cleanup_typed_file::<ComponentConfiguration>(path, "ComponentConfiguration")
            }
            FileKind::ModuleManifest => {
                self.cleanup_typed_file::<ModuleManifest>(path, "ModuleManifest")
            }
        }
    }

    fn cleanup_dir_json<T: serde::de::DeserializeOwned + Serialize>(
        &mut self,
        dir: &Path,
        schema: &str,
    ) -> crate::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(());
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            self.cleanup_typed_file::<T>(&path, schema)?;
        }

        Ok(())
    }

    fn cleanup_typed_file<T: serde::de::DeserializeOwned + Serialize>(
        &mut self,
        path: &Path,
        schema: &str,
    ) -> crate::Result<()> {
        self.files_considered += 1;

        let raw = match read_json_file(path) {
            Ok(v) => v,
            Err(err) => {
                self.skipped.push(DoctorCleanupSkipped {
                    file: path.to_string_lossy().to_string(),
                    reason: format!("read_json_error: {}", err),
                });
                return Ok(());
            }
        };

        let typed: T = match serde_json::from_value(raw.clone()) {
            Ok(v) => v,
            Err(err) => {
                self.skipped.push(DoctorCleanupSkipped {
                    file: path.to_string_lossy().to_string(),
                    reason: format!("SCHEMA_DESERIALIZE_ERROR: {}", err),
                });
                return Ok(());
            }
        };

        let mut scanner = Scanner::new("doctor.cleanup");
        let unknown = scanner.unknown_top_level_keys(path, schema, &raw, &typed);
        if unknown.is_empty() {
            return Ok(());
        }

        let Some(mut raw_obj) = raw.as_object().cloned() else {
            self.skipped.push(DoctorCleanupSkipped {
                file: path.to_string_lossy().to_string(),
                reason: format!("SCHEMA_TYPE_ERROR: Expected JSON object for {}", schema),
            });
            return Ok(());
        };

        for key in &unknown {
            raw_obj.remove(key);
        }

        if !self.dry_run {
            write_json_file_pretty(path, &Value::Object(raw_obj.clone()))?;
        }

        self.changes.push(DoctorCleanupChange {
            file: path.to_string_lossy().to_string(),
            schema: schema.to_string(),
            removed_keys: unknown,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_are_detected() {
        let mut scanner = Scanner::new("doctor.scan");
        let raw = serde_json::json!({
            "defaultDatabaseHost": "127.0.0.1",
            "unknownField": 123
        });
        let typed = AppConfig::default();
        let path = Path::new("/tmp/config.json");
        scanner.emit_unknown_keys(path, "AppConfig", &raw, &typed);

        assert!(scanner
            .issues
            .iter()
            .any(|i| i.code == "UNKNOWN_KEYS" && i.severity == DoctorSeverity::Warning));
    }

    #[test]
    fn scan_command_is_standardized() {
        let result = Doctor::scan(DoctorScope::All).unwrap();
        assert_eq!(result.report.command, "doctor.scan");
    }

    #[test]
    fn cleanup_refuses_unknown_file_kind() {
        let result = Doctor::cleanup_file(Path::new("/tmp/not-homeboy.json"), true);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn cleanup_dry_run_reports_changes_without_writing() {
        let dir = std::env::temp_dir().join("homeboy-doctor-cleanup-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("config.json");
        let original = serde_json::json!({
            "defaultDatabaseHost": "192.168.1.1",
            "extra": 1
        });
        write_json_file_pretty(&path, &original).unwrap();

        let result = Doctor::cleanup_file(&path, true).unwrap();
        assert_eq!(result.cleanup.command, "doctor.cleanup");
        assert!(result.cleanup.summary.dry_run);
        assert_eq!(result.cleanup.summary.files_changed, 1);
        assert_eq!(result.cleanup.summary.keys_removed, 1);

        let after = read_json_file(&path).unwrap();
        assert!(after.get("extra").is_some());
    }

}
