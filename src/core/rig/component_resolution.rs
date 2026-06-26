//! Rig component resolution helpers.

use crate::core::component::{self, Component};
use crate::core::error::{Error, Result};
use crate::core::expand;

use super::spec::{ComponentSpec, RigSpec};

pub fn resolve_component_path(rig: &RigSpec, component_id: &str) -> Result<String> {
    resolve_component(rig, component_id).map(|component| component.local_path)
}

pub fn resolve_component(rig: &RigSpec, component_id: &str) -> Result<Component> {
    let spec = rig.components.get(component_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "components",
            format!(
                "component '{component_id}' not declared in rig '{}'",
                rig.id
            ),
            Some(component_id.to_string()),
            None,
        )
    })?;

    let registry_id = spec
        .component_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(component_id);

    let mut attempts = Vec::new();

    if let Some(path) = super::expand::component_path_override_from_env(&rig.id, component_id) {
        attempts.push(format!(
            "{} override env",
            super::expand::rig_component_path_override_env_name(&rig.id, component_id)
        ));
        return Ok(component_from_spec(component_id, spec, path, None));
    }

    let explicit_path = expand_component_path(rig, &spec.path);
    if !explicit_path.trim().is_empty() {
        attempts.push("path".to_string());
        return Ok(component_from_spec(component_id, spec, explicit_path, None));
    }
    if !spec.path.trim().is_empty() {
        attempts.push("path expanded to an empty value".to_string());
    }

    attempts.push(format!("component registry id `{registry_id}`"));
    if let Ok(mut registered) = component::resolve_effective(Some(registry_id), None, None) {
        apply_rig_component_overrides(component_id, spec, &mut registered);
        return Ok(registered);
    }

    if let Some(path) = path_from_path_setting(spec) {
        attempts.push(format!(
            "path_setting `{}`",
            spec.path_setting.as_deref().unwrap_or_default()
        ));
        return Ok(component_from_spec(
            component_id,
            spec,
            path,
            Some(registry_id),
        ));
    }
    if let Some(path_setting) = spec.path_setting.as_deref() {
        attempts.push(format!("path_setting `{path_setting}` was unset or empty"));
    }

    Err(Error::validation_invalid_argument(
        "components",
        format!(
            "rig '{}' could not resolve component '{}' to a local path",
            rig.id, component_id
        ),
        Some(component_id.to_string()),
        Some(attempts),
    ))
}

fn component_from_spec(
    component_id: &str,
    spec: &ComponentSpec,
    local_path: String,
    registry_id: Option<&str>,
) -> Component {
    let mut component = Component {
        id: registry_id.unwrap_or(component_id).to_string(),
        local_path,
        remote_url: spec.remote_url.clone(),
        triage_remote_url: spec.triage_remote_url.clone(),
        extensions: spec.extensions.clone(),
        ..Component::default()
    };
    component.resolve_remote_path();
    component
}

fn apply_rig_component_overrides(
    component_id: &str,
    spec: &ComponentSpec,
    component: &mut Component,
) {
    if component.id.is_empty() {
        component.id = component_id.to_string();
    }
    if let Some(remote_url) = spec.remote_url.clone() {
        component.remote_url = Some(remote_url);
    }
    if let Some(triage_remote_url) = spec.triage_remote_url.clone() {
        component.triage_remote_url = Some(triage_remote_url);
    }
    if let Some(extensions) = spec.extensions.clone() {
        component.extensions = Some(extensions);
    }
    component.resolve_remote_path();
}

fn path_from_path_setting(spec: &ComponentSpec) -> Option<String> {
    let setting = spec.path_setting.as_deref()?.trim();
    if setting.is_empty() {
        return None;
    }
    let value = std::env::var(setting).ok()?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(expand::expand_with_tilde(value, |_| None))
}

fn expand_component_path(rig: &RigSpec, path: &str) -> String {
    if path.trim().is_empty() {
        return String::new();
    }
    expand::expand_with_tilde(path, |token| {
        if token == "package.root" {
            if let Some(package_root) = super::local_package_root(&rig.id) {
                return Some(package_root.to_string_lossy().to_string());
            }
            return super::install::read_source_metadata(&rig.id)
                .map(|metadata| metadata.package_path);
        }
        token
            .strip_prefix("env.")
            .map(|name| std::env::var(name).unwrap_or_default())
    })
}

pub fn component_ref(spec: &ComponentSpec) -> Option<String> {
    spec.r#ref.clone().or_else(|| spec.default_ref.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;

    fn rig_with_component(component: ComponentSpec) -> RigSpec {
        RigSpec {
            id: "sample-rig".to_string(),
            components: HashMap::from([("app".to_string(), component)]),
            ..RigSpec::default()
        }
    }

    fn component_spec() -> ComponentSpec {
        ComponentSpec {
            path: String::new(),
            component_id: None,
            path_setting: None,
            checkout_root: None,
            remote_url: None,
            triage_remote_url: None,
            stack: None,
            branch: None,
            r#ref: None,
            default_ref: None,
            extensions: None,
        }
    }

    #[test]
    fn resolves_component_path_from_registry_id() {
        with_isolated_home(|home| {
            let checkout = tempfile::tempdir().expect("checkout");
            let components = home.path().join(".config/homeboy/components");
            std::fs::create_dir_all(&components).expect("components dir");
            std::fs::write(
                components.join("registry-app.json"),
                serde_json::json!({ "local_path": checkout.path() }).to_string(),
            )
            .expect("component registration");

            let mut spec = component_spec();
            spec.component_id = Some("registry-app".to_string());
            let rig = rig_with_component(spec);

            let path = resolve_component_path(&rig, "app").expect("component path");
            assert_eq!(path, checkout.path().to_string_lossy());
        });
    }

    #[test]
    fn unresolved_component_reports_attempted_sources() {
        with_isolated_home(|_home| {
            let old_setting = std::env::var("HOMEBOY_TEST_APP_PATH").ok();
            std::env::remove_var("HOMEBOY_TEST_APP_PATH");

            let mut spec = component_spec();
            spec.component_id = Some("missing-app".to_string());
            spec.path_setting = Some("HOMEBOY_TEST_APP_PATH".to_string());
            let rig = rig_with_component(spec);

            let error = resolve_component_path(&rig, "app").unwrap_err();
            let rendered = format!("{error:?}");
            assert!(rendered.contains("missing-app"));
            assert!(rendered.contains("HOMEBOY_TEST_APP_PATH"));

            match old_setting {
                Some(value) => std::env::set_var("HOMEBOY_TEST_APP_PATH", value),
                None => std::env::remove_var("HOMEBOY_TEST_APP_PATH"),
            }
        });
    }

    #[test]
    fn default_ref_falls_back_when_ref_is_omitted() {
        let mut spec = component_spec();
        spec.default_ref = Some("origin/main".to_string());
        assert_eq!(component_ref(&spec).as_deref(), Some("origin/main"));
        spec.r#ref = Some("abc123".to_string());
        assert_eq!(component_ref(&spec).as_deref(), Some("abc123"));
    }
}
