//! Extension manifest capability/config types (deploy, build, test, lint, cli,
//! database, discovery, requirements, source-snapshot).

use crate::{TestDriftConfig, TestPassthroughFilter};
use homeboy_engine_primitives::output_parse::ParseSpec;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// An extension-owned executable readiness probe for portable operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolchainReadinessProbe {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostic_env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homeboy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvProviderConfig {
    /// Script path relative to the extension directory.
    ///
    /// The script runs with the same generic Homeboy execution context as the
    /// target command and prints a JSON object of environment variables to add.
    pub script: String,
    /// Secret names the provider expects the runner to resolve through its
    /// existing secret-env references. Values never travel in provider plans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshotConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_excludes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<DatabaseCliConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseCliConfig {
    pub tables_command: String,
    pub describe_command: String,
    pub query_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliHelpConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id_help: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_help: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliConfig {
    pub tool: String,
    pub display_name: String,
    pub command_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cli_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir_template: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings_flags: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_flags: Vec<CliAutoFlag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<CliHelpConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAutoFlag {
    #[serde(default)]
    pub when: CliAutoFlagCondition,
    pub flag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliAutoFlagCondition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    pub find_command: String,
    pub base_path_transform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployVerification {
    pub path_pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_error_message: Option<String>,
}

fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployOverride {
    pub path_pattern: String,
    #[serde(default = "default_staging_path")]
    pub staging_path: String,
    pub install_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_command: Option<String>,
    #[serde(default)]
    pub skip_permissions_fix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployOwnerHint {
    pub path_contains: String,
    pub suggested_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePathInferenceRule {
    pub when_file_contains: FileContainsCondition,
    pub remote_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotePathRootRule {
    pub path_prefix: String,
    pub root: String,
    #[serde(default)]
    pub strip_prefix: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detect_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileContainsCondition {
    pub file: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionPatternConfig {
    pub extension: String,
    pub pattern: String,
}

/// Configuration for replacing `@since` placeholder tags during version bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinceTagConfig {
    /// File extensions to scan.
    pub extensions: Vec<String>,
    /// Regex pattern matching placeholder versions in `@since` tags.
    /// Default: `0\.0\.0|NEXT|TBD|TODO|UNRELEASED|x\.x\.x`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_build_script: Option<String>,
    /// Optional provider-owned resolver for `homeboy build --changed-since`.
    ///
    /// The script receives `HOMEBOY_CHANGED_SINCE` and reports whether the
    /// provider can skip, scope, or must run a full build. Core treats missing
    /// or inconclusive resolver output as a conservative full build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_scope_script: Option<String>,
    /// Default artifact path pattern with template support.
    /// Supports: {component_id}, {local_path}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_pattern: Option<String>,
    /// Paths to clean up after successful deploy (e.g., node_modules, vendor, target)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_paths: Vec<String>,
    /// Repo-relative paths to lockfiles this extension's build process
    /// regenerates.
    ///
    /// These are merge-aftermath drift on the base branch: a release version
    /// bump can cause extension-managed dependency metadata to refresh. The CI
    /// autofix pipeline treats lockfile drift the same as audit baseline drift:
    /// it's pushed directly to the base branch instead of opened as a
    /// reviewable PR.
    ///
    /// Paths are repo-root-relative. Absolute paths are rejected. Existence
    /// is the caller's responsibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lockfile_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,

    /// Changed-file routing rules for split lint runners.
    ///
    /// When present, changed-file lint scopes files to the matching runner step
    /// selectors instead of passing every changed file through one invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_file_routes: Vec<LintChangedFileRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LintChangedFileRoute {
    /// File extensions matched without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,

    /// Glob patterns matched against component-relative file paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,

    /// Extension runner step selector.
    pub step: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_parse: Option<ParseSpec>,
    /// Source/test selection contract used by changed-test and drift workflows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<TestDriftConfig>,

    /// Manifest-driven routing for changed-test selections before invoking the
    /// extension test runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_file_routing: Option<TestChangedFileRouting>,

    /// Manifest-driven mapping for Homeboy's generic `--filter` passthrough
    /// hint before invoking the extension test runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passthrough_filter: Option<TestPassthroughFilter>,

    /// Explicit extension-owned policy for an intentional no-test scope.
    /// The extension must write a nonce-bound result envelope to the
    /// invocation-provided evidence file before Homeboy accepts a neutral result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_tests_applicable: Option<TestNoTestsApplicablePolicy>,

    /// Environment the test command may carry to a portable runner. Exact keys
    /// and prefixes are opt-in so unrelated controller environment never leaks.
    #[serde(default, skip_serializing_if = "PortableEnvConfig::is_empty")]
    pub portable_env: PortableEnvConfig,

    /// Maps the environment name consumed by the test runner to the selected
    /// runner's existing `secret_env` identity. These are references only;
    /// secret values are resolved by the runner at execution time.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, String>,

    /// Conditionally project secret identity names from validated component
    /// settings. The selected settings leaf must be an object whose values are
    /// environment identity names; resolved secret values never enter settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_projections: Vec<TestSecretEnvProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestSecretEnvProjection {
    pub when: TestSettingStringPredicate,
    pub names_path: Vec<String>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestSettingStringPredicate {
    pub path: Vec<String>,
    pub equals: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableEnvConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefixes: Vec<String>,
}

impl PortableEnvConfig {
    pub const MAX_ENTRIES: usize = 64;
    pub const MAX_NAME_LEN: usize = 128;

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.prefixes.is_empty()
    }

    pub fn validate(&self) -> homeboy_error::Result<()> {
        if self.keys.len() + self.prefixes.len() > Self::MAX_ENTRIES {
            return Err(homeboy_error::Error::validation_invalid_argument(
                "test.portable_env",
                format!(
                    "must declare at most {} keys and prefixes",
                    Self::MAX_ENTRIES
                ),
                None,
                None,
            ));
        }
        for (kind, names) in [("keys", &self.keys), ("prefixes", &self.prefixes)] {
            for name in names {
                let valid = !name.is_empty()
                    && name.len() <= Self::MAX_NAME_LEN
                    && name
                        .chars()
                        .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
                    && name
                        .chars()
                        .next()
                        .is_some_and(|ch| ch == '_' || ch.is_ascii_uppercase());
                if !valid {
                    return Err(homeboy_error::Error::validation_invalid_argument(
                        format!("test.portable_env.{kind}"),
                        "must contain non-empty ASCII uppercase environment identifiers up to 128 characters",
                        Some(name.clone()),
                        None,
                    ));
                }
                if looks_like_secret_env_name(name) {
                    return Err(homeboy_error::Error::validation_invalid_argument(
                        format!("test.portable_env.{kind}"),
                        "must not declare secret-looking names; declare runner-resolved references in test.secret_env instead",
                        Some(name.clone()),
                        None,
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn validate_test_secret_env_references(
    references: &BTreeMap<String, String>,
) -> homeboy_error::Result<()> {
    for (name, reference) in references {
        for value in [name, reference] {
            let valid = !value.is_empty()
                && value.len() <= PortableEnvConfig::MAX_NAME_LEN
                && value
                    .chars()
                    .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
                && value
                    .chars()
                    .next()
                    .is_some_and(|ch| ch == '_' || ch.is_ascii_uppercase());
            if !valid {
                return Err(homeboy_error::Error::validation_invalid_argument(
                    "test.secret_env",
                    "must map ASCII uppercase environment identities to runner secret_env identities",
                    Some(format!("{name}={reference}")),
                    None,
                ));
            }
        }
        if name != reference {
            return Err(homeboy_error::Error::validation_invalid_argument(
                "test.secret_env",
                "must map each test environment name to the same existing runner secret_env identity",
                Some(format!("{name}={reference}")),
                Some(vec![
                    "Configure the runner's secret_env reference under the environment name consumed by the test runner."
                        .to_string(),
                ]),
            ));
        }
    }
    Ok(())
}

pub fn validate_test_secret_env_projections(
    projections: &[TestSecretEnvProjection],
) -> homeboy_error::Result<()> {
    if projections.len() > PortableEnvConfig::MAX_ENTRIES {
        return Err(homeboy_error::Error::validation_invalid_argument(
            "test.secret_env_projections",
            format!(
                "must declare at most {} projections",
                PortableEnvConfig::MAX_ENTRIES
            ),
            None,
            None,
        ));
    }

    for projection in projections {
        validate_settings_path(&projection.when.path, "when.path")?;
        validate_settings_path(&projection.names_path, "names_path")?;
        if projection.when.equals.is_empty()
            || projection.when.equals.len() > PortableEnvConfig::MAX_NAME_LEN
        {
            return Err(homeboy_error::Error::validation_invalid_argument(
                "test.secret_env_projections.when.equals",
                "must be a non-empty string up to 128 characters",
                None,
                None,
            ));
        }
    }
    Ok(())
}

fn validate_settings_path(path: &[String], field: &str) -> homeboy_error::Result<()> {
    let valid = !path.is_empty()
        && path.len() <= 16
        && path.iter().all(|segment| {
            !segment.is_empty()
                && segment.len() <= PortableEnvConfig::MAX_NAME_LEN
                && segment != "."
                && segment != ".."
        });
    if valid {
        return Ok(());
    }
    Err(homeboy_error::Error::validation_invalid_argument(
        format!("test.secret_env_projections.{field}"),
        "must contain 1 to 16 non-empty settings object path segments",
        None,
        None,
    ))
}

fn looks_like_secret_env_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "PASSWORD",
        "PASSWD",
        "SECRET",
        "TOKEN",
        "API_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestNoTestsApplicablePolicy {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestChangedFileRouting {
    pub strategy: TestChangedFileRoutingStrategy,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_env: Option<TestChangedFileExclusiveEnv>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TestChangedFileRoutingStrategy {
    FileArgs,
    RustCargo,
    ExclusiveEnv,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestChangedFileExclusiveEnv {
    /// Environment variable to set when all selected tests match this route.
    pub name: String,

    /// Glob patterns matched against component-relative test paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,

    /// File extensions matched without leading dots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        validate_test_secret_env_projections, validate_test_secret_env_references,
        PortableEnvConfig, TestSecretEnvProjection, TestSettingStringPredicate,
    };
    use std::collections::BTreeMap;

    #[test]
    fn portable_env_accepts_exact_keys_and_prefixes() {
        PortableEnvConfig {
            keys: vec!["DB_SERVICE_HOST".to_string()],
            prefixes: vec!["DB_SERVICE_".to_string()],
        }
        .validate()
        .expect("portable environment contract");
    }

    #[test]
    fn portable_env_rejects_non_identifier_and_unbounded_declarations() {
        assert!(PortableEnvConfig {
            keys: vec!["db-service".to_string()],
            prefixes: Vec::new(),
        }
        .validate()
        .is_err());
        assert!(PortableEnvConfig {
            keys: (0..=PortableEnvConfig::MAX_ENTRIES)
                .map(|index| format!("DB_SERVICE_{index}"))
                .collect(),
            prefixes: Vec::new(),
        }
        .validate()
        .is_err());
    }

    #[test]
    fn portable_env_rejects_secret_looking_public_names_with_secret_reference_guidance() {
        let error = PortableEnvConfig {
            keys: vec!["DB_SERVICE_PASSWORD".to_string()],
            prefixes: Vec::new(),
        }
        .validate()
        .expect_err("password must not be captured as public environment");
        assert!(error.message.contains("test.secret_env"));

        validate_test_secret_env_references(&BTreeMap::from([(
            "DB_SERVICE_PASSWORD".to_string(),
            "DB_SERVICE_PASSWORD".to_string(),
        )]))
        .expect("runner secret reference");
    }

    #[test]
    fn conditional_secret_projection_rejects_malformed_paths() {
        let projection = TestSecretEnvProjection {
            when: TestSettingStringPredicate {
                path: vec!["service".to_string(), "provider".to_string()],
                equals: "remote".to_string(),
            },
            names_path: vec!["service".to_string(), "secret_env".to_string()],
            optional: false,
        };
        validate_test_secret_env_projections(&[projection.clone()])
            .expect("bounded projection");

        for malformed in [
            TestSecretEnvProjection {
                names_path: Vec::new(),
                ..projection.clone()
            },
            TestSecretEnvProjection {
                when: TestSettingStringPredicate {
                    path: vec!["service".to_string(), "".to_string()],
                    equals: "remote".to_string(),
                },
                ..projection.clone()
            },
            TestSecretEnvProjection {
                when: TestSettingStringPredicate {
                    equals: String::new(),
                    ..projection.when.clone()
                },
                ..projection.clone()
            },
        ] {
            assert!(validate_test_secret_env_projections(&[malformed]).is_err());
        }
    }

    #[test]
    fn conditional_secret_projection_rejects_unsupported_predicates_and_value_fields() {
        for malformed in [
            serde_json::json!({
                "when":{"path":["service","mode"],"operator":"equals","value":"remote"},
                "names_path":["service","secret_env"]
            }),
            serde_json::json!({
                "when":{"path":["service","mode"],"equals":"remote"},
                "names_path":["service","secret_env"],
                "value_path":["service","password"]
            }),
        ] {
            assert!(serde_json::from_value::<TestSecretEnvProjection>(malformed).is_err());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_script: Option<String>,
}
