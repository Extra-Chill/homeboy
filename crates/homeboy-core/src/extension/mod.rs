pub mod audit_compiler_warning_provider;
pub mod audit_fingerprint_script_provider;
pub mod audit_grammar_source_provider;
pub mod audit_manifest_provider;
pub mod bench;
pub mod build;
pub mod catalog;
pub mod component_script;
pub mod fingerprint;
pub mod invoke;
pub mod lifecycle;
pub mod lint;
mod manifest_sidecar;
pub mod readiness;
pub mod recipe_run;
mod refactor_protocol;
pub mod registry;
pub mod resolve;
pub mod self_check;
mod setup_env;
pub mod test;
pub mod trace;

pub(crate) use homeboy_core::extension::resolve::{extension_guidance_hints, stderr_tail};

pub(crate) use invoke::build_settings_json_from_manifest;
pub use refactor_protocol::{
    run_refactor_script, run_refactor_script_result, AdjustedItem, ParsedItem,
    RefactorScriptFailure, RefactorScriptFailureKind, RelatedTests, ResolvedImports,
    RewrittenImport,
};

#[cfg(test)]
mod tests;
