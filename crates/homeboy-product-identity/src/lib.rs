use std::path::PathBuf;

/// Homeboy-owned product literals that core code needs for config, paths, and
/// backward-compatible environment contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductIdentity {
    pub name: &'static str,
    pub binary_name: &'static str,
    pub config_filename: &'static str,
    pub config_dirname: &'static str,
    pub data_dirname: &'static str,
    pub env_prefix: &'static str,
    pub artifact_prefix: &'static str,
    pub run_dir_prefix: &'static str,
}

pub const PRODUCT_IDENTITY: ProductIdentity = ProductIdentity {
    name: "Homeboy",
    binary_name: "homeboy",
    config_filename: "homeboy.json",
    config_dirname: "homeboy",
    data_dirname: "homeboy",
    env_prefix: "HOMEBOY_",
    artifact_prefix: ".homeboy-",
    run_dir_prefix: "homeboy-run",
};

impl ProductIdentity {
    pub fn env_var(self, suffix: &str) -> String {
        format!("{}{}", self.env_prefix, suffix)
    }

    pub fn config_file(self, base: PathBuf) -> PathBuf {
        base.join(self.config_filename)
    }
}
