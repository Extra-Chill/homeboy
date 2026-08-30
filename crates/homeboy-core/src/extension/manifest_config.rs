pub use homeboy_extension_contract::autofix_config::AutofixVerifyConfig;
pub use homeboy_extension_contract::trace_config::{
    TraceBrowserArtifactMapConfig, TraceBrowserEvidenceAdapterConfig,
    TraceBrowserMetricAliasConfig, TraceBrowserSummaryAliasConfig, TraceConfig,
    TraceToolchainProvenanceConfig,
};

#[cfg(test)]
mod tests {
    use homeboy_extension_contract::manifest_toolchain_config::DepsConfig;

    #[test]
    fn deps_config_preserves_the_legacy_extension_script_contract() {
        let config: DepsConfig = serde_json::from_str(r#"{"extension_script":"deps.sh"}"#).unwrap();
        assert_eq!(config.extension_script.as_deref(), Some("deps.sh"));
    }
}
