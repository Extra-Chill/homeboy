pub mod bench;
pub mod build;
pub mod catalog;
pub mod component_script;
pub mod deployment_api;
pub mod external_check_detail_api;
pub mod invoke;
pub mod lifecycle;
pub mod lint;
mod manifest_sidecar;
pub mod readiness;
pub mod recipe_run_api;
pub mod resolve;
pub(crate) mod root_manifest;
pub mod self_check;
mod setup_env;
pub mod test;
pub mod trace;
pub mod workload;

pub(crate) use homeboy_core::extension::resolve::{extension_guidance_hints, stderr_tail};

pub(crate) use invoke::build_settings_json;

#[cfg(test)]
mod tests;
