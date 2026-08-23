//! Rig-side implementation of core's rig provider hooks.
//!
//! Supplies rig data to core's HTTP API and scope resolution, and the toolchain
//! command-step PATH, so those core surfaces work without depending on the rig
//! subsystem directly.

use std::path::Path;

use homeboy_core::component::{self, Component};
use homeboy_core::paths;
use homeboy_core::rig_provider::{register_rig_provider, RigProvider};
use homeboy_core::rig_toolchain_provider::{register_rig_toolchain_provider, RigToolchainProvider};
use homeboy_core::scope::ScopeComponentRef;
use homeboy_core::Result;
use serde_json::{json, Value};

/// Each provider method answers one HTTP request, which is one unit of work,
/// so this is a boundary resolution rather than an ambient one (#7505). It is
/// per-call: the provider is a long-lived singleton and capturing a root at
/// registration would pin it to whatever home happened to be current then.
fn config_root() -> Result<std::path::PathBuf> {
    paths::homeboy()
}

struct RigProviderImpl;

impl RigProvider for RigProviderImpl {
    fn rig_list_json(&self) -> Result<Value> {
        Ok(json!(crate::list(&config_root()?)?))
    }

    fn rig_show_json(&self, id: &str) -> Result<Value> {
        Ok(json!(crate::load(&config_root()?, id)?))
    }

    fn rig_check_json(&self, id: &str) -> Result<Value> {
        let rig = crate::load(&config_root()?, id)?;
        Ok(json!(crate::runner::run_check(&rig)?))
    }

    fn rig_ids(&self) -> Result<Vec<String>> {
        Ok(crate::list(&config_root()?)?
            .into_iter()
            .map(|rig| rig.id)
            .collect())
    }

    fn resolve_rig_scope(&self, rig_id: &str) -> Result<Vec<ScopeComponentRef>> {
        let spec = crate::load(&config_root()?, rig_id)?;
        let mut refs = Vec::new();
        for (component_id, component_spec) in spec.components.iter() {
            let path = crate::expand::expand_vars(&spec, &component_spec.path);
            let mut component_ref = ScopeComponentRef::new(
                component_id.clone(),
                path,
                component_spec.remote_url.clone(),
                component_spec.triage_remote_url.clone(),
                format!("rig:{rig_id}"),
            );
            component_ref.usage.insert(rig_id.to_string());
            refs.push(component_ref);
        }
        refs.sort_by(|a, b| a.component_id.cmp(&b.component_id));
        Ok(refs)
    }

    fn resolve_rig_component_records(&self, rig_id: &str) -> Result<Vec<Component>> {
        let spec = crate::load(&config_root()?, rig_id)?;
        let mut components = Vec::new();
        for (component_id, component_spec) in spec.components.iter() {
            let local_path = crate::expand::expand_vars(&spec, &component_spec.path);
            let mut component = component::discover_from_portable(Path::new(&local_path))
                .or_else(|| component::load(component_id).ok())
                .unwrap_or_default();
            component.id = component_id.clone();
            component.local_path = local_path;
            if component.remote_url.is_none() {
                component.remote_url = component_spec.remote_url.clone();
            }
            if component.triage_remote_url.is_none() {
                component.triage_remote_url = component_spec.triage_remote_url.clone();
            }
            if component.extensions.is_none() {
                component.extensions = component_spec.extensions.clone();
            }
            components.push(component);
        }
        components.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(components)
    }
}

struct RigToolchainProviderImpl;

impl RigToolchainProvider for RigToolchainProviderImpl {
    fn command_step_path(&self, rig_id: Option<&str>) -> Option<std::ffi::OsString> {
        // A named rig contributes its own `toolchain` declaration. An unknown or
        // unloadable id degrades to the built-in default rather than failing an
        // exec-env build over PATH assembly.
        let rig = rig_id
            .and_then(|rig_id| config_root().ok().map(|root| (root, rig_id)))
            .and_then(|(root, rig_id)| crate::load(&root, rig_id).ok());
        crate::toolchain::command_step_path(rig.as_ref())
    }
}

/// Register the rig providers so core's HTTP API, scope resolution, and
/// extension exec-env builder work without depending on the rig subsystem.
pub fn register() {
    register_rig_provider(Box::new(RigProviderImpl));
    register_rig_toolchain_provider(Box::new(RigToolchainProviderImpl));
}
