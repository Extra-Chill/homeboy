//! Clap argument definitions for the `agent-task auth` subcommand tree.

use clap::{Args, Subcommand};

#[derive(Subcommand, Debug)]
pub enum AgentTaskAuthCommand {
    /// Show redacted readiness for provider secret environment variables.
    Status(AgentTaskAuthStatusArgs),
    /// Store a provider secret in the OS keychain and map it to a required env name.
    SetKeychain(AgentTaskAuthSetKeychainArgs),
    /// Store a provider secret in Homeboy global config and map it to a required env name.
    SetConfig(AgentTaskAuthSetConfigArgs),
    /// Store a JSON secret bundle in one OS keychain item.
    SetKeychainBundle(AgentTaskAuthSetKeychainBundleArgs),
    /// Map a required provider env name to another process env var.
    MapEnv(AgentTaskAuthMapEnvArgs),
    /// Map a required provider env name to a field in a JSON keychain bundle.
    MapKeychainBundle(AgentTaskAuthMapKeychainBundleArgs),
    /// Remove a provider secret source mapping.
    Remove(AgentTaskAuthRemoveArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthStatusArgs {
    /// Executor backend whose required secrets to report. Defaults to the same
    /// backend cook/dispatch would use when omitted.
    #[arg(long = "backend", value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Provider id to disambiguate when more than one provider exists for the backend.
    #[arg(
        long = "selector",
        visible_alias = "provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,

    /// Secret environment variable name to check without exposing its value.
    /// Repeatable. When omitted, the selected backend's required secrets are used.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthSetKeychainArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Secret value. Omit to prompt securely.
    #[arg(value_name = "VALUE")]
    pub value: Option<String>,

    /// Read the secret value from stdin.
    #[arg(long)]
    pub value_stdin: bool,

    /// Keychain scope. Defaults to agent-task.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Keychain entry name. Defaults to ENV.
    #[arg(long = "name", value_name = "NAME")]
    pub keychain_name: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthSetConfigArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Secret value. Omit to prompt securely.
    #[arg(value_name = "VALUE")]
    pub value: Option<String>,

    /// Read the secret value from stdin.
    #[arg(long)]
    pub value_stdin: bool,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthSetKeychainBundleArgs {
    /// Logical bundle id to store.
    #[arg(value_name = "BUNDLE")]
    pub bundle: String,

    /// JSON bundle value. Omit to prompt securely.
    #[arg(value_name = "JSON")]
    pub value: Option<String>,

    /// Read the JSON bundle value from stdin.
    #[arg(long)]
    pub value_stdin: bool,

    /// Keychain scope. Defaults to agent-task.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Keychain entry name. Defaults to BUNDLE.
    #[arg(long = "name", value_name = "NAME")]
    pub keychain_name: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthMapEnvArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Source process environment variable. Defaults to ENV.
    #[arg(long = "from", value_name = "ENV")]
    pub source_env: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthMapKeychainBundleArgs {
    /// Required provider environment variable name to satisfy.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Logical bundle id to read.
    #[arg(long, value_name = "BUNDLE")]
    pub bundle: String,

    /// Field path inside the JSON bundle, using dots for nested objects.
    #[arg(long, value_name = "FIELD")]
    pub field: String,

    /// Keychain scope. Defaults to agent-task.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Keychain entry name. Defaults to BUNDLE.
    #[arg(long = "name", value_name = "NAME")]
    pub keychain_name: Option<String>,
}

#[derive(Args, Debug)]
pub struct AgentTaskAuthRemoveArgs {
    /// Required provider environment variable name whose mapping should be removed.
    #[arg(value_name = "ENV")]
    pub secret_env: String,

    /// Also remove the mapped keychain entry when the mapping points at keychain.
    #[arg(long)]
    pub keychain: bool,
}
