use homeboy_core::config::{
    ComponentConfiguration, InstalledModuleConfig, ModuleScope, ProjectConfiguration,
    ScopedModuleConfig,
};
use homeboy_core::module::{ModuleManifest, SettingConfig};
use serde_json::json;
use std::collections::HashMap;

fn module_manifest() -> ModuleManifest {
    ModuleManifest {
        id: "m".to_string(),
        name: "Module".to_string(),
        version: "0.1.0".to_string(),
        icon: "icon".to_string(),
        description: Some("desc".to_string()),
        author: Some("me".to_string()),
        homepage: None,
        config_schema: None,
        default_pinned_files: Vec::new(),
        default_pinned_logs: Vec::new(),
        database: None,
        cli: None,
        discovery: None,
        deploy: Vec::new(),
        version_patterns: Vec::new(),
        build: None,
        commands: Vec::new(),
        runtime: Some(homeboy_core::module::RuntimeConfig {
            runtime_type: homeboy_core::module::RuntimeType::Cli,
            entrypoint: None,
            dependencies: None,
            playwright_browsers: None,
            args: Some("echo".to_string()),
            default_site: None,
        }),
        inputs: Vec::new(),
        output: Some(homeboy_core::module::OutputConfig {
            schema: homeboy_core::module::OutputSchema {
                schema_type: "object".to_string(),
                items: None,
            },
            display: "".to_string(),
            selectable: false,
        }),
        actions: Vec::new(),
        settings: vec![
            SettingConfig {
                id: "a".to_string(),
                setting_type: "string".to_string(),
                label: "A".to_string(),
                placeholder: None,
                default: Some(json!("default")),
            },
            SettingConfig {
                id: "n".to_string(),
                setting_type: "number".to_string(),
                label: "N".to_string(),
                placeholder: None,
                default: Some(json!(1)),
            },
        ],
        requires: None,
        module_path: None,
    }
}

#[test]
fn merges_with_precedence_and_defaults() {
    let module = module_manifest();

    let mut installed = InstalledModuleConfig::default();
    installed.settings.insert("a".to_string(), json!("app"));

    let mut project = ProjectConfiguration {
        name: "p".to_string(),
        domain: "d".to_string(),
        modules: vec!["wordpress".to_string()],
        scoped_modules: None,
        server_id: None,
        base_path: None,
        table_prefix: None,
        module_settings: Default::default(),
        remote_files: Default::default(),
        remote_logs: Default::default(),
        database: Default::default(),
        local_environment: Default::default(),
        tools: Default::default(),
        api: Default::default(),
        changelog_next_section_label: None,
        changelog_next_section_aliases: None,
        sub_targets: Vec::new(),
        shared_tables: Vec::new(),
        component_ids: Vec::new(),
    };

    let mut project_scoped_modules = HashMap::new();
    let mut project_scoped = ScopedModuleConfig::default();
    project_scoped
        .settings
        .insert("a".to_string(), json!("project"));
    project_scoped_modules.insert("m".to_string(), project_scoped);
    project.scoped_modules = Some(project_scoped_modules);

    let mut component = ComponentConfiguration::new(
        "c".to_string(),
        "C".to_string(),
        ".".to_string(),
        ".".to_string(),
        "a".to_string(),
    );

    let mut component_scoped_modules = HashMap::new();
    let mut component_scoped = ScopedModuleConfig::default();
    component_scoped
        .settings
        .insert("a".to_string(), json!("component"));
    component_scoped_modules.insert("m".to_string(), component_scoped);
    component.scoped_modules = Some(component_scoped_modules);

    let out = ModuleScope::effective_settings_validated(
        &module,
        Some(&installed),
        Some(&project),
        Some(&component),
    )
    .unwrap();

    assert_eq!(out.get("a"), Some(&json!("component")));
    assert_eq!(out.get("n"), Some(&json!(1)));
}

#[test]
fn rejects_unknown_setting_key() {
    let module = module_manifest();

    let mut installed = InstalledModuleConfig::default();
    installed.settings.insert("nope".to_string(), json!("x"));

    let err = ModuleScope::effective_settings_validated(&module, Some(&installed), None, None)
        .unwrap_err();

    assert_eq!(err.code.as_str(), "config.invalid_value");
}

#[test]
fn rejects_invalid_type() {
    let module = module_manifest();

    let mut installed = InstalledModuleConfig::default();
    installed
        .settings
        .insert("n".to_string(), json!("not-a-number"));

    let err = ModuleScope::effective_settings_validated(&module, Some(&installed), None, None)
        .unwrap_err();

    assert_eq!(err.code.as_str(), "config.invalid_value");
}
