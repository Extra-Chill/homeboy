use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::env_materialization_plan::{EnvMaterializationPlan, EnvSecretRef, EnvSourceEnvBinding};
use homeboy_redaction::RedactionPolicy;

pub const SECRET_ENV_PLAN_SCHEMA: &str = "homeboy/secret-env-plan/v1";
pub const SECRET_ENV_PLAN_MATERIALIZATION_SCHEMA: &str =
    "homeboy/secret-env-plan-materialization/v1";
pub const SECRET_ENV_MATERIALIZED_HANDOFF_SCHEMA: &str =
    "homeboy/secret-env-materialized-handoff/v1";
pub const AGENT_TASK_SECRET_ENV_PLAN_JSON_ENV: &str = "HOMEBOY_AGENT_TASK_SECRET_ENV_PLAN_JSON";
pub const SECRET_ENV_PLAN_ENV_DELTA_SOURCE: &str = "secret_env_plan_env_delta";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlan {
    #[serde(default = "secret_env_plan_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub public_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<SecretEnvRequirement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_credentials: BTreeMap<String, SecretEnvProviderCredentialMapping>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_name_mapping: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
    #[serde(
        default,
        skip_serializing_if = "SecretEnvRedactionPolicy::is_default_policy"
    )]
    pub redaction: SecretEnvRedactionPolicy,
    #[serde(
        default,
        skip_serializing_if = "SecretEnvInheritancePolicy::is_default_policy"
    )]
    pub inheritance: SecretEnvInheritancePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_materialization: Option<EnvMaterializationPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvRequirement {
    pub name: String,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<SecretEnvRefreshHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvRefreshHint {
    pub provider: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlanDiagnostics {
    pub passthrough_secret_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_env: Vec<SecretEnvInheritedEnvStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvDiagnosticStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvInheritedEnvStatus {
    pub name: String,
    pub declared: bool,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvDiagnosticStatus {
    pub name: String,
    pub required: bool,
    pub configured: bool,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_source_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<SecretEnvRefreshHint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlanDiagnosticError {
    pub missing_required_secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undeclared_inherited_secret_env: Vec<String>,
    pub message: String,
    pub diagnostics: SecretEnvPlanDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SecretEnvPlanMaterializeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_plan: Option<SecretEnvPlan>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub public_env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirements: Vec<SecretEnvRequirement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_env_map: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_name_mapping: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_allowed_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inheritance: Option<SecretEnvInheritancePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<SecretEnvRedactionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvPlanMaterialization {
    pub schema: String,
    pub valid: bool,
    pub plan: SecretEnvPlan,
    pub diagnostics: SecretEnvPlanDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvStatus {
    pub name: String,
    pub configured: bool,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_env_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_source_env_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvResolution {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvResolutionError {
    pub missing_secret_env: Vec<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvMaterializedHandoff {
    #[serde(default = "secret_env_materialized_handoff_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<SecretEnvMaterializedHandoffEnv>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_secret_env_names: Vec<String>,
    pub diagnostics: SecretEnvPlanDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvMaterializedHandoffRequest {
    pub plan: SecretEnvPlan,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<SecretEnvStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvMaterializedHandoffEnv {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_env_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvHandoffEntry {
    pub name: String,
    pub owner: String,
    pub source: String,
    pub destination: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

pub struct SecretEnvValueProvider<'a> {
    source: String,
    resolve: Box<dyn FnMut(&str) -> Option<String> + 'a>,
}

impl<'a> SecretEnvValueProvider<'a> {
    pub fn new(
        source: impl Into<String>,
        resolve: impl FnMut(&str) -> Option<String> + 'a,
    ) -> Self {
        Self {
            source: source.into(),
            resolve: Box::new(resolve),
        }
    }
}

pub fn resolve_secret_env_names(
    names: impl IntoIterator<Item = String>,
    providers: Vec<SecretEnvValueProvider<'_>>,
    missing_message_prefix: &str,
) -> Result<SecretEnvResolution, SecretEnvResolutionError> {
    SecretEnvResolver::new(providers).resolve_required(names, missing_message_prefix)
}

pub struct SecretEnvResolver<'a> {
    providers: Vec<SecretEnvValueProvider<'a>>,
}

impl<'a> SecretEnvResolver<'a> {
    pub fn new(providers: Vec<SecretEnvValueProvider<'a>>) -> Self {
        Self { providers }
    }

    pub fn resolve_required(
        mut self,
        names: impl IntoIterator<Item = String>,
        missing_message_prefix: &str,
    ) -> Result<SecretEnvResolution, SecretEnvResolutionError> {
        let names = normalize_names(names);
        let mut env = Vec::new();
        let mut status = Vec::new();
        let mut missing = Vec::new();

        for name in names {
            let mut resolved = None;
            for provider in self.providers.iter_mut() {
                if let Some(value) = (provider.resolve)(&name) {
                    resolved = Some((provider.source.clone(), value));
                    break;
                }
            }

            if let Some((source, value)) = resolved {
                env.push((name.clone(), value));
                status.push(secret_env_status(name, true, source));
            } else {
                status.push(secret_env_status(name.clone(), false, "missing"));
                missing.push(name);
            }
        }

        if missing.is_empty() {
            Ok(SecretEnvResolution { env, status })
        } else {
            Err(SecretEnvResolutionError {
                message: format!("{missing_message_prefix}: {}", missing.join(", ")),
                missing_secret_env: missing,
                status,
            })
        }
    }

    pub fn resolve_plan(
        mut self,
        plan: &SecretEnvPlan,
        missing_message_prefix: &str,
    ) -> Result<SecretEnvResolution, SecretEnvResolutionError> {
        let mut env = Vec::new();
        let mut status = Vec::new();
        let mut missing = Vec::new();

        for requirement in plan.secret_env_requirements() {
            let source_env_names = requirement.source_env_candidates();
            let mut resolved = None;
            let mut missing_source_env_names = Vec::new();

            for source_env_name in &source_env_names {
                let mut source_resolved = None;
                for provider in self.providers.iter_mut() {
                    if let Some(value) = (provider.resolve)(source_env_name) {
                        source_resolved = Some((provider.source.clone(), value));
                        break;
                    }
                }

                if let Some((source, value)) = source_resolved {
                    resolved = Some((source_env_name.clone(), source, value));
                    break;
                }

                missing_source_env_names.push(source_env_name.clone());
            }

            if let Some((source_env_name, source, value)) = resolved {
                env.push((requirement.name.clone(), value));
                status.push(secret_env_status_with_sources(
                    requirement.name,
                    true,
                    source,
                    Some(source_env_name),
                    missing_source_env_names,
                ));
            } else {
                status.push(secret_env_status_with_sources(
                    requirement.name.clone(),
                    false,
                    "missing",
                    None,
                    missing_source_env_names,
                ));
                if requirement.required {
                    missing.push(requirement.name);
                }
            }
        }

        if missing.is_empty() {
            Ok(SecretEnvResolution { env, status })
        } else {
            Err(SecretEnvResolutionError {
                message: format!("{missing_message_prefix}: {}", missing.join(", ")),
                missing_secret_env: missing,
                status,
            })
        }
    }
}

impl Default for SecretEnvPlan {
    fn default() -> Self {
        Self {
            schema: SECRET_ENV_PLAN_SCHEMA.to_string(),
            public_env: BTreeMap::new(),
            secret_env_names: Vec::new(),
            requirements: Vec::new(),
            provider_credentials: BTreeMap::new(),
            env_name_mapping: BTreeMap::new(),
            status: Vec::new(),
            redaction: SecretEnvRedactionPolicy::default(),
            inheritance: SecretEnvInheritancePolicy::default(),
            env_materialization: None,
        }
    }
}

impl SecretEnvPlan {
    /// Project this secret-env plan into a public `EnvMaterializationPlan`.
    ///
    /// Lives here (rather than as `EnvMaterializationPlan::from_secret_env_plan`)
    /// so `env_materialization_plan` carries no dependency on `secret_env_plan` —
    /// the dependency flows one way, `secret_env_plan -> env_materialization_plan`.
    pub fn to_env_materialization_plan(&self) -> EnvMaterializationPlan {
        let mut env_plan = EnvMaterializationPlan {
            public_env: self.public_env.clone(),
            secret_refs: normalize_names(self.secret_env_names())
                .into_iter()
                .map(|name| EnvSecretRef { name, owner: None })
                .collect(),
            source_env_bindings: self
                .secret_env_requirements()
                .into_iter()
                .filter_map(|requirement| {
                    let source_env_refs = normalize_names(requirement.source_env_candidates());
                    if source_env_refs.is_empty() {
                        None
                    } else {
                        Some(EnvSourceEnvBinding {
                            name: requirement.name,
                            source_env_refs,
                        })
                    }
                })
                .collect(),
            inherited_allowed_env_names: normalize_names(
                self.inheritance.allowed_env_names.clone(),
            ),
            ..EnvMaterializationPlan::default()
        };
        env_plan.normalize();
        env_plan
    }

    pub fn materialize_request(
        request: SecretEnvPlanMaterializeRequest,
    ) -> Result<SecretEnvPlanMaterialization, SecretEnvPlanDiagnosticError> {
        let mut plan = request.base_plan.unwrap_or_default();
        plan.schema = SECRET_ENV_PLAN_SCHEMA.to_string();
        plan.public_env.extend(request.public_env);
        plan.extend_secret_env_names(request.secret_env_names);

        if !request.requirements.is_empty() {
            plan.requirements =
                normalize_requirements(plan.requirements.into_iter().chain(request.requirements));
        }

        for (name, source_env_names) in request.source_env_map {
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            let source_env_names = normalize_ordered_names(source_env_names);
            upsert_requirement(
                &mut plan.requirements,
                SecretEnvRequirement {
                    name: name.clone(),
                    required: true,
                    source_env_names,
                    refresh: None,
                },
            );
            plan.extend_secret_env_names([name]);
        }

        for (key, names) in request.env_name_mapping {
            plan.map_env_names(key, names);
        }

        if let Some(inheritance) = request.inheritance {
            plan.inheritance = SecretEnvInheritancePolicy {
                require_declaration: inheritance.require_declaration,
                allowed_env_names: normalize_names(
                    inheritance
                        .allowed_env_names
                        .into_iter()
                        .chain(request.inherited_allowed_env_names),
                ),
            };
        } else {
            plan.allow_inherited_env_names(request.inherited_allowed_env_names);
        }

        if let Some(redaction) = request.redaction {
            plan.redaction = redaction;
        }

        plan.requirements = normalize_requirements(plan.secret_env_requirements());

        let diagnostics = plan.diagnose_materialized_declarations(
            request.inherited_env_names,
            request
                .diagnostic_source
                .unwrap_or_else(|| "materialize-request".to_string()),
        )?;

        Ok(SecretEnvPlanMaterialization {
            schema: SECRET_ENV_PLAN_MATERIALIZATION_SCHEMA.to_string(),
            valid: true,
            plan,
            diagnostics,
        })
    }

    pub fn from_secret_env_names(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            secret_env_names: normalize_names(names),
            ..Self::default()
        }
    }

    pub fn from_requirements(requirements: impl IntoIterator<Item = SecretEnvRequirement>) -> Self {
        let mut plan = Self::default();
        plan.requirements = normalize_requirements(requirements);
        plan.secret_env_names = plan
            .requirements
            .iter()
            .map(|requirement| requirement.name.clone())
            .collect();
        plan
    }

    pub fn merge_from(&mut self, plan: SecretEnvPlan) {
        self.public_env.extend(plan.public_env);
        self.extend_secret_env_names(plan.secret_env_names);
        self.requirements =
            normalize_requirements(self.requirements.iter().cloned().chain(plan.requirements));
        self.provider_credentials.extend(plan.provider_credentials);
        for (key, names) in plan.env_name_mapping {
            self.map_env_names(key, names);
        }
        self.status = normalize_status(self.status.iter().cloned().chain(plan.status));
        if plan.redaction != SecretEnvRedactionPolicy::default() {
            self.redaction = plan.redaction;
        }
        if plan.inheritance != SecretEnvInheritancePolicy::default() {
            self.inheritance = plan.inheritance;
        }
        if plan.env_materialization.is_some() {
            self.env_materialization = plan.env_materialization;
        }
    }

    pub fn json_env_pair(&self) -> (String, String) {
        (
            AGENT_TASK_SECRET_ENV_PLAN_JSON_ENV.to_string(),
            serde_json::to_string(self).unwrap_or_else(|_| "null".to_string()),
        )
    }

    pub fn secret_env_names(&self) -> Vec<String> {
        let mapped_names = self
            .provider_credentials
            .values()
            .flat_map(|mapping| mapping.secret_env.iter().cloned());
        let env_name_mapping_names = self
            .env_name_mapping
            .values()
            .flat_map(|names| names.iter().cloned());
        normalize_names(
            self.secret_env_names
                .iter()
                .cloned()
                .chain(self.requirements.iter().flat_map(|requirement| {
                    std::iter::once(requirement.name.clone())
                        .chain(requirement.source_env_names.iter().cloned())
                }))
                .chain(mapped_names)
                .chain(env_name_mapping_names),
        )
    }

    pub fn diagnose(
        &self,
        status: impl IntoIterator<Item = SecretEnvStatus>,
    ) -> Result<SecretEnvPlanDiagnostics, SecretEnvPlanDiagnosticError> {
        let status_by_name = status
            .into_iter()
            .map(|status| (status.name.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let requirements = self.secret_env_requirements();
        let passthrough_secret_env_names = requirements
            .iter()
            .map(|requirement| requirement.name.clone())
            .collect::<Vec<_>>();
        let mut missing_required_secret_env = Vec::new();
        let mut diagnostic_status = Vec::new();

        for requirement in requirements {
            let status = status_by_name
                .get(&requirement.name)
                .cloned()
                .unwrap_or_else(|| secret_env_status(requirement.name.clone(), false, "missing"));
            if requirement.required && !status.configured {
                missing_required_secret_env.push(requirement.name.clone());
            }
            let source_env_names = requirement.source_env_candidates();
            diagnostic_status.push(SecretEnvDiagnosticStatus {
                name: requirement.name,
                required: requirement.required,
                configured: status.configured,
                source: status.source,
                source_env_names,
                missing_source_env_names: status.missing_source_env_names,
                refresh: requirement.refresh,
            });
        }

        let diagnostics = SecretEnvPlanDiagnostics {
            passthrough_secret_env_names,
            inherited_env: Vec::new(),
            status: diagnostic_status,
        };

        if missing_required_secret_env.is_empty() {
            Ok(diagnostics)
        } else {
            Err(SecretEnvPlanDiagnosticError {
                message: format!(
                    "missing required secret env: {}",
                    missing_required_secret_env.join(", ")
                ),
                missing_required_secret_env,
                undeclared_inherited_secret_env: Vec::new(),
                diagnostics,
            })
        }
    }

    pub fn diagnose_inherited_env(
        &self,
        env: &BTreeMap<String, String>,
        source: impl Into<String>,
    ) -> Result<SecretEnvPlanDiagnostics, SecretEnvPlanDiagnosticError> {
        let source = source.into();
        let declared = self.declared_inherited_env_names();
        let policy = self.redaction.to_policy();
        let mut undeclared = Vec::new();
        let mut inherited_env = Vec::new();

        for name in env.keys() {
            if !policy.is_sensitive_key(name) {
                continue;
            }
            let is_declared = declared.contains(name);
            if self.inheritance.require_declaration && !is_declared {
                undeclared.push(name.clone());
            }
            inherited_env.push(SecretEnvInheritedEnvStatus {
                name: name.clone(),
                declared: is_declared,
                source: source.clone(),
            });
        }

        let mut diagnostics = self
            .diagnose(self.status.clone())
            .unwrap_or_else(|error| error.diagnostics);
        diagnostics.inherited_env = inherited_env;

        if undeclared.is_empty() {
            Ok(diagnostics)
        } else {
            Err(SecretEnvPlanDiagnosticError {
                message: format!("undeclared inherited secret env: {}", undeclared.join(", ")),
                missing_required_secret_env: Vec::new(),
                undeclared_inherited_secret_env: undeclared,
                diagnostics,
            })
        }
    }

    pub fn diagnose_inherited_env_names(
        &self,
        names: impl IntoIterator<Item = String>,
        source: impl Into<String>,
    ) -> Result<SecretEnvPlanDiagnostics, SecretEnvPlanDiagnosticError> {
        let env = normalize_names(names)
            .into_iter()
            .map(|name| (name, String::new()))
            .collect::<BTreeMap<_, _>>();
        self.diagnose_inherited_env(&env, source)
    }

    fn diagnose_materialized_declarations(
        &self,
        inherited_env_names: impl IntoIterator<Item = String>,
        inherited_source: impl Into<String>,
    ) -> Result<SecretEnvPlanDiagnostics, SecretEnvPlanDiagnosticError> {
        let status = self
            .secret_env_requirements()
            .into_iter()
            .map(|requirement| {
                secret_env_status_with_sources(requirement.name, true, "declared", None, Vec::new())
            })
            .collect::<Vec<_>>();
        let mut diagnostics = self.diagnose(status)?;
        let inherited = self.diagnose_inherited_env_names(inherited_env_names, inherited_source);
        match inherited {
            Ok(inherited_diagnostics) => {
                diagnostics.inherited_env = inherited_diagnostics.inherited_env;
                Ok(diagnostics)
            }
            Err(mut error) => {
                error.diagnostics.status = diagnostics.status;
                error.diagnostics.passthrough_secret_env_names =
                    diagnostics.passthrough_secret_env_names;
                Err(error)
            }
        }
    }

    pub fn remove_undeclared_inherited_secret_env(
        &self,
        env: &mut HashMap<String, String>,
    ) -> Vec<String> {
        let declared = self.declared_inherited_env_names();
        let policy = self.redaction.to_policy();
        let mut removed = env
            .keys()
            .filter(|name| policy.is_sensitive_key(name) && !declared.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        removed.sort();
        removed.dedup();

        for name in &removed {
            env.remove(name);
        }

        removed
    }

    pub fn secret_env_requirements(&self) -> Vec<SecretEnvRequirement> {
        let mut requirements = self
            .requirements
            .iter()
            .cloned()
            .map(|mut requirement| {
                requirement.name = requirement.name.trim().to_string();
                requirement
            })
            .filter(|requirement| !requirement.name.is_empty())
            .map(|requirement| (requirement.name.clone(), requirement))
            .collect::<BTreeMap<_, _>>();

        let mapped_names = self
            .provider_credentials
            .values()
            .flat_map(|mapping| mapping.secret_env.iter().cloned());
        let env_name_mapping_names = self
            .env_name_mapping
            .values()
            .flat_map(|names| names.iter().cloned());

        for name in normalize_names(
            self.secret_env_names
                .iter()
                .cloned()
                .chain(mapped_names)
                .chain(env_name_mapping_names),
        ) {
            requirements
                .entry(name.clone())
                .or_insert_with(|| SecretEnvRequirement {
                    name,
                    required: true,
                    source_env_names: Vec::new(),
                    refresh: None,
                });
        }

        requirements.into_values().collect()
    }

    pub fn extend_secret_env_names(&mut self, names: impl IntoIterator<Item = String>) {
        self.secret_env_names = normalize_names(self.secret_env_names.iter().cloned().chain(names));
    }

    pub fn allow_inherited_env_names(&mut self, names: impl IntoIterator<Item = String>) {
        self.inheritance.allowed_env_names = normalize_names(
            self.inheritance
                .allowed_env_names
                .iter()
                .cloned()
                .chain(names),
        );
    }

    pub fn map_env_names(
        &mut self,
        key: impl Into<String>,
        names: impl IntoIterator<Item = String>,
    ) {
        let names = normalize_names(names);
        if !names.is_empty() {
            self.env_name_mapping.insert(key.into(), names);
        }
    }

    pub fn with_status(mut self, status: impl IntoIterator<Item = SecretEnvStatus>) -> Self {
        self.status = status.into_iter().collect();
        self
    }

    pub fn materialize(
        &self,
        secret_values: impl IntoIterator<Item = (String, String)>,
    ) -> BTreeMap<String, String> {
        let mut env = self.public_env.clone();
        for (name, value) in secret_values {
            env.insert(name, value);
        }
        env
    }

    pub fn redacted_env(&self) -> BTreeMap<String, String> {
        let policy = self.redaction.to_policy();
        let mut env = self
            .public_env
            .iter()
            .map(|(name, value)| (name.clone(), policy.redact_env_value(value)))
            .collect::<BTreeMap<_, _>>();
        for name in self.secret_env_names() {
            env.insert(name, self.redaction.replacement.clone());
        }
        env
    }

    pub fn redacted(&self) -> Self {
        let policy = self.redaction.to_policy();
        let mut redacted = self.clone();
        redacted.public_env = self
            .public_env
            .iter()
            .map(|(name, value)| (name.clone(), policy.redact_env_value(value)))
            .collect();
        redacted
    }

    pub fn materialized_handoff(
        &self,
        status: impl IntoIterator<Item = SecretEnvStatus>,
    ) -> SecretEnvMaterializedHandoff {
        SecretEnvMaterializedHandoff::from_status(self, status)
    }

    fn declared_inherited_env_names(&self) -> BTreeSet<String> {
        self.secret_env_names()
            .into_iter()
            .chain(self.public_env.keys().cloned())
            .chain(self.inheritance.allowed_env_names.iter().cloned())
            .collect()
    }
}

impl SecretEnvMaterializedHandoff {
    pub fn materialize_request(request: SecretEnvMaterializedHandoffRequest) -> Self {
        Self::from_status(&request.plan, request.status)
    }

    pub fn from_resolution(plan: &SecretEnvPlan, resolution: &SecretEnvResolution) -> Self {
        Self::from_status(plan, resolution.status.clone())
    }

    pub fn from_error(plan: &SecretEnvPlan, error: &SecretEnvResolutionError) -> Self {
        Self::from_status(plan, error.status.clone())
    }

    pub fn from_status(
        plan: &SecretEnvPlan,
        status: impl IntoIterator<Item = SecretEnvStatus>,
    ) -> Self {
        let status = normalize_status(status);
        let diagnostics = plan
            .diagnose(status.clone())
            .unwrap_or_else(|error| error.diagnostics);
        let missing_secret_env_names = diagnostics
            .status
            .iter()
            .filter(|entry| entry.required && !entry.configured)
            .map(|entry| entry.name.clone())
            .collect();
        let env = status
            .into_iter()
            .filter(|entry| entry.configured)
            .map(|entry| SecretEnvMaterializedHandoffEnv {
                name: entry.name,
                source: entry.source,
                source_env_name: entry.source_env_name,
            })
            .collect();

        Self {
            schema: SECRET_ENV_MATERIALIZED_HANDOFF_SCHEMA.to_string(),
            env,
            missing_secret_env_names,
            diagnostics,
        }
    }
}

fn upsert_requirement(
    requirements: &mut Vec<SecretEnvRequirement>,
    requirement: SecretEnvRequirement,
) {
    let mut merged = requirements
        .drain(..)
        .map(|entry| (entry.name.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    merged
        .entry(requirement.name.clone())
        .and_modify(|entry| {
            entry.required = entry.required || requirement.required;
            entry.source_env_names = normalize_ordered_names(
                entry
                    .source_env_names
                    .iter()
                    .cloned()
                    .chain(requirement.source_env_names.iter().cloned()),
            );
            if entry.refresh.is_none() {
                entry.refresh = requirement.refresh.clone();
            }
        })
        .or_insert(requirement);
    *requirements = merged.into_values().collect();
}

impl SecretEnvRequirement {
    pub fn source_env_candidates(&self) -> Vec<String> {
        if self.source_env_names.is_empty() {
            vec![self.name.clone()]
        } else {
            self.source_env_names.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvInheritancePolicy {
    #[serde(default = "default_require_declaration")]
    pub require_declaration: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_env_names: Vec<String>,
}

impl Default for SecretEnvInheritancePolicy {
    fn default() -> Self {
        Self {
            require_declaration: default_require_declaration(),
            allowed_env_names: Vec::new(),
        }
    }
}

impl SecretEnvInheritancePolicy {
    fn is_default_policy(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvProviderCredentialMapping {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, SecretEnvCredentialSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvCredentialSource {
    #[serde(default = "default_secret_env_source")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretEnvRedactionPolicy {
    #[serde(default = "default_replacement")]
    pub replacement: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensitive_env_names: Vec<String>,
}

impl Default for SecretEnvRedactionPolicy {
    fn default() -> Self {
        Self {
            replacement: default_replacement(),
            sensitive_env_names: Vec::new(),
        }
    }
}

impl SecretEnvRedactionPolicy {
    fn is_default_policy(&self) -> bool {
        self == &Self::default()
    }

    fn to_policy(&self) -> RedactionPolicy {
        self.sensitive_env_names.iter().fold(
            RedactionPolicy::default().with_replacement(self.replacement.clone()),
            |policy, name| policy.with_sensitive_key(name),
        )
    }
}

pub fn normalize_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalize_requirements(
    requirements: impl IntoIterator<Item = SecretEnvRequirement>,
) -> Vec<SecretEnvRequirement> {
    requirements
        .into_iter()
        .map(|mut requirement| {
            requirement.name = requirement.name.trim().to_string();
            requirement.source_env_names = normalize_ordered_names(requirement.source_env_names);
            requirement
        })
        .filter(|requirement| !requirement.name.is_empty())
        .map(|requirement| (requirement.name.clone(), requirement))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

fn normalize_status(status: impl IntoIterator<Item = SecretEnvStatus>) -> Vec<SecretEnvStatus> {
    status
        .into_iter()
        .map(|mut status| {
            status.name = status.name.trim().to_string();
            status.missing_source_env_names =
                normalize_ordered_names(status.missing_source_env_names);
            status
        })
        .filter(|status| !status.name.is_empty())
        .map(|status| (status.name.clone(), status))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

fn secret_env_status(name: String, configured: bool, source: impl Into<String>) -> SecretEnvStatus {
    secret_env_status_with_sources(name, configured, source, None, Vec::new())
}

fn secret_env_status_with_sources(
    name: String,
    configured: bool,
    source: impl Into<String>,
    source_env_name: Option<String>,
    missing_source_env_names: Vec<String>,
) -> SecretEnvStatus {
    SecretEnvStatus {
        name,
        configured,
        source: source.into(),
        source_env_name,
        missing_source_env_names,
    }
}

fn normalize_ordered_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for name in names {
        let name = name.trim().to_string();
        if !name.is_empty() && seen.insert(name.clone()) {
            normalized.push(name);
        }
    }
    normalized
}

fn secret_env_plan_schema() -> String {
    SECRET_ENV_PLAN_SCHEMA.to_string()
}

fn secret_env_materialized_handoff_schema() -> String {
    SECRET_ENV_MATERIALIZED_HANDOFF_SCHEMA.to_string()
}

fn default_secret_env_source() -> String {
    "env".to_string()
}

fn default_replacement() -> String {
    "[REDACTED]".to_string()
}

fn default_required() -> bool {
    true
}

fn default_require_declaration() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_env_materialization_plan_carries_source_env_refs_without_values() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "SERVICE_TOKEN".to_string(),
            required: true,
            source_env_names: vec!["SOURCE_SERVICE_TOKEN".to_string()],
            refresh: None,
        }]);

        let env_plan = plan.to_env_materialization_plan();
        let json = serde_json::to_string(&env_plan).expect("serializes env materialization plan");

        assert!(json.contains("SERVICE_TOKEN"));
        assert!(json.contains("SOURCE_SERVICE_TOKEN"));
        assert!(!json.contains("secret-value"));
        assert_eq!(
            env_plan.source_env_bindings,
            vec![EnvSourceEnvBinding {
                name: "SERVICE_TOKEN".to_string(),
                source_env_refs: vec!["SOURCE_SERVICE_TOKEN".to_string()],
            }]
        );
    }

    #[test]
    fn secret_env_plan_materializes_public_and_secret_values() {
        let plan = SecretEnvPlan {
            public_env: BTreeMap::from([("PUBLIC_FLAG".to_string(), "1".to_string())]),
            secret_env_names: vec!["API_TOKEN".to_string()],
            ..SecretEnvPlan::default()
        };

        let env = plan.materialize([("API_TOKEN".to_string(), "secret-value".to_string())]);

        assert_eq!(env.get("PUBLIC_FLAG"), Some(&"1".to_string()));
        assert_eq!(env.get("API_TOKEN"), Some(&"secret-value".to_string()));
    }

    #[test]
    fn secret_env_plan_redacts_secret_values_without_storing_them() {
        let mut provider_credentials = BTreeMap::new();
        provider_credentials.insert(
            "example-oauth".to_string(),
            SecretEnvProviderCredentialMapping {
                secret_env: vec!["EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string()],
                sources: BTreeMap::from([(
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
                    SecretEnvCredentialSource {
                        source: "keychain-bundle".to_string(),
                        scope: Some("agent-task".to_string()),
                        name: Some("example-oauth".to_string()),
                        field: Some("refresh_token".to_string()),
                        env_var: None,
                    },
                )]),
            },
        );
        let plan = SecretEnvPlan {
            public_env: BTreeMap::from([(
                "PUBLIC_ENDPOINT".to_string(),
                "https://example.test/?token=abc&ok=1".to_string(),
            )]),
            provider_credentials,
            ..SecretEnvPlan::default()
        };

        let serialized = serde_json::to_string(&plan).expect("plan json");
        let redacted = plan.redacted_env();

        assert!(!serialized.contains("secret-value"));
        assert_eq!(
            redacted.get("PUBLIC_ENDPOINT"),
            Some(&"https://example.test/?token=[REDACTED]&ok=1".to_string())
        );
        assert_eq!(
            redacted.get("EXAMPLE_PROVIDER_REFRESH_TOKEN"),
            Some(&"[REDACTED]".to_string())
        );
    }

    #[test]
    fn secret_env_plan_deduplicates_direct_and_provider_secret_names() {
        let plan = SecretEnvPlan {
            secret_env_names: vec!["B_SECRET".to_string(), "A_SECRET".to_string()],
            provider_credentials: BTreeMap::from([(
                "provider".to_string(),
                SecretEnvProviderCredentialMapping {
                    secret_env: vec!["A_SECRET".to_string(), "C_SECRET".to_string()],
                    sources: BTreeMap::new(),
                },
            )]),
            ..SecretEnvPlan::default()
        };

        assert_eq!(
            plan.secret_env_names(),
            vec![
                "A_SECRET".to_string(),
                "B_SECRET".to_string(),
                "C_SECRET".to_string()
            ]
        );
    }

    #[test]
    fn secret_env_plan_merge_preserves_contract_fields() {
        let mut target = SecretEnvPlan::from_secret_env_names(["EXPLICIT_SECRET".to_string()]);
        target
            .public_env
            .insert("PUBLIC".to_string(), "1".to_string());

        target.merge_from(SecretEnvPlan {
            public_env: BTreeMap::from([("PUBLIC_FROM_PLAN".to_string(), "2".to_string())]),
            requirements: vec![SecretEnvRequirement {
                name: "PROVIDER_TOKEN".to_string(),
                required: true,
                source_env_names: vec!["SOURCE_TOKEN".to_string()],
                refresh: Some(SecretEnvRefreshHint {
                    provider: "provider-auth".to_string(),
                    metadata: BTreeMap::from([("account".to_string(), "default".to_string())]),
                }),
            }],
            provider_credentials: BTreeMap::from([(
                "provider".to_string(),
                SecretEnvProviderCredentialMapping {
                    secret_env: vec!["PROVIDER_REFRESH".to_string()],
                    sources: BTreeMap::new(),
                },
            )]),
            env_name_mapping: BTreeMap::from([(
                "runtime".to_string(),
                vec!["RUNTIME_SECRET".to_string()],
            )]),
            inheritance: SecretEnvInheritancePolicy {
                require_declaration: true,
                allowed_env_names: vec!["HOMEBOY_RUNTIME_SECRET_ENV".to_string()],
            },
            ..SecretEnvPlan::default()
        });

        assert_eq!(target.public_env.get("PUBLIC"), Some(&"1".to_string()));
        assert_eq!(
            target.public_env.get("PUBLIC_FROM_PLAN"),
            Some(&"2".to_string())
        );
        assert_eq!(
            target.secret_env_names(),
            vec![
                "EXPLICIT_SECRET".to_string(),
                "PROVIDER_REFRESH".to_string(),
                "PROVIDER_TOKEN".to_string(),
                "RUNTIME_SECRET".to_string(),
                "SOURCE_TOKEN".to_string()
            ]
        );
        assert_eq!(
            target.requirements[0]
                .refresh
                .as_ref()
                .map(|hint| hint.provider.as_str()),
            Some("provider-auth")
        );
        assert!(target.provider_credentials.contains_key("provider"));
        assert_eq!(
            target.inheritance.allowed_env_names,
            vec!["HOMEBOY_RUNTIME_SECRET_ENV".to_string()]
        );
    }

    #[test]
    fn secret_env_plan_includes_mapped_env_names() {
        let mut plan = SecretEnvPlan::from_secret_env_names(["DIRECT_SECRET".to_string()]);
        plan.map_env_names(
            "provider.example",
            ["MAPPED_SECRET".to_string(), "DIRECT_SECRET".to_string()],
        );

        assert_eq!(
            plan.secret_env_names(),
            vec!["DIRECT_SECRET".to_string(), "MAPPED_SECRET".to_string()]
        );
        assert_eq!(
            plan.env_name_mapping.get("provider.example"),
            Some(&vec![
                "DIRECT_SECRET".to_string(),
                "MAPPED_SECRET".to_string()
            ])
        );
    }

    #[test]
    fn secret_env_plan_json_env_pair_uses_canonical_handoff_name() {
        let plan = SecretEnvPlan::from_secret_env_names(["API_TOKEN".to_string()]);

        let (name, value) = plan.json_env_pair();

        assert_eq!(name, AGENT_TASK_SECRET_ENV_PLAN_JSON_ENV);
        let decoded: SecretEnvPlan = serde_json::from_str(&value).expect("secret plan json");
        assert_eq!(decoded.secret_env_names(), vec!["API_TOKEN".to_string()]);
    }

    #[test]
    fn normalizes_secret_env_names_by_trimming_sorting_and_deduplicating() {
        assert_eq!(
            normalize_names([
                " B_SECRET ".to_string(),
                "".to_string(),
                "A_SECRET".to_string(),
                "B_SECRET".to_string(),
            ]),
            vec!["A_SECRET".to_string(), "B_SECRET".to_string()]
        );
    }

    #[test]
    fn resolver_returns_values_and_redacted_status_contract() {
        let resolver = SecretEnvResolver::new(vec![
            SecretEnvValueProvider::new("missing-source", |_| None),
            SecretEnvValueProvider::new("test-source", |name| {
                (name == "API_TOKEN").then(|| "secret-value".to_string())
            }),
        ]);

        let resolved = resolver
            .resolve_required(vec!["API_TOKEN".to_string()], "missing required secret env")
            .expect("secret resolves");

        assert_eq!(
            resolved.env,
            vec![("API_TOKEN".to_string(), "secret-value".to_string())]
        );
        assert_eq!(
            resolved.status,
            vec![SecretEnvStatus {
                name: "API_TOKEN".to_string(),
                configured: true,
                source: "test-source".to_string(),
                source_env_name: None,
                missing_source_env_names: Vec::new(),
            }]
        );
        assert!(!serde_json::to_string(&resolved.status)
            .expect("status json")
            .contains("secret-value"));
    }

    #[test]
    fn resolver_reports_missing_names_without_values() {
        let resolver =
            SecretEnvResolver::new(vec![SecretEnvValueProvider::new("test-source", |_| None)]);

        let error = resolver
            .resolve_required(
                vec!["B_SECRET".to_string(), "A_SECRET".to_string()],
                "missing required secret env",
            )
            .expect_err("secrets are missing");

        assert_eq!(
            error.missing_secret_env,
            vec!["A_SECRET".to_string(), "B_SECRET".to_string()]
        );
        assert_eq!(
            error.message,
            "missing required secret env: A_SECRET, B_SECRET"
        );
        assert!(!serde_json::to_string(&error)
            .expect("error json")
            .contains("secret-value"));
    }

    #[test]
    fn plan_resolver_maps_requirement_to_multiple_source_env_fallbacks() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "API_TOKEN".to_string(),
            required: true,
            source_env_names: vec![
                "PRIMARY_API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
            ],
            refresh: None,
        }]);
        let resolver =
            SecretEnvResolver::new(vec![SecretEnvValueProvider::new("test-env", |name| {
                (name == "FALLBACK_API_TOKEN").then(|| "secret-value".to_string())
            })]);

        let resolved = resolver
            .resolve_plan(&plan, "missing required secret env")
            .expect("fallback source resolves");

        assert_eq!(
            resolved.env,
            vec![("API_TOKEN".to_string(), "secret-value".to_string())]
        );
        assert_eq!(
            resolved.status,
            vec![SecretEnvStatus {
                name: "API_TOKEN".to_string(),
                configured: true,
                source: "test-env".to_string(),
                source_env_name: Some("FALLBACK_API_TOKEN".to_string()),
                missing_source_env_names: vec!["PRIMARY_API_TOKEN".to_string()],
            }]
        );
        assert_eq!(
            plan.secret_env_names(),
            vec![
                "API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
                "PRIMARY_API_TOKEN".to_string(),
            ]
        );
        assert!(!serde_json::to_string(&resolved.status)
            .expect("status json")
            .contains("secret-value"));
    }

    #[test]
    fn materialized_handoff_projects_env_keys_sources_and_redacted_diagnostics_only() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "API_TOKEN".to_string(),
            required: true,
            source_env_names: vec![
                "PRIMARY_API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
            ],
            refresh: None,
        }]);
        let resolver =
            SecretEnvResolver::new(vec![SecretEnvValueProvider::new("test-env", |name| {
                (name == "FALLBACK_API_TOKEN").then(|| "secret-value".to_string())
            })]);

        let resolved = resolver
            .resolve_plan(&plan, "missing required secret env")
            .expect("fallback source resolves");
        let handoff = SecretEnvMaterializedHandoff::from_resolution(&plan, &resolved);
        let serialized = serde_json::to_string(&handoff).expect("handoff json");

        assert_eq!(handoff.schema, SECRET_ENV_MATERIALIZED_HANDOFF_SCHEMA);
        assert_eq!(
            handoff.env,
            vec![SecretEnvMaterializedHandoffEnv {
                name: "API_TOKEN".to_string(),
                source: "test-env".to_string(),
                source_env_name: Some("FALLBACK_API_TOKEN".to_string()),
            }]
        );
        assert!(handoff.missing_secret_env_names.is_empty());
        assert_eq!(
            handoff.diagnostics.status[0].missing_source_env_names,
            vec!["PRIMARY_API_TOKEN".to_string()]
        );
        assert!(serialized.contains("API_TOKEN"));
        assert!(serialized.contains("test-env"));
        assert!(!serialized.contains("secret-value"));
    }

    #[test]
    fn materialized_handoff_reports_missing_names_without_values() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "API_TOKEN".to_string(),
            required: true,
            source_env_names: vec!["SOURCE_API_TOKEN".to_string()],
            refresh: None,
        }]);
        let resolver =
            SecretEnvResolver::new(vec![SecretEnvValueProvider::new("test-env", |_| {
                None::<String>
            })]);

        let error = resolver
            .resolve_plan(&plan, "missing required secret env")
            .expect_err("secret is missing");
        let handoff = plan.materialized_handoff(error.status.clone());
        let from_error = SecretEnvMaterializedHandoff::from_error(&plan, &error);
        let serialized = serde_json::to_string(&handoff).expect("handoff json");

        assert_eq!(handoff, from_error);
        assert!(handoff.env.is_empty());
        assert_eq!(
            handoff.missing_secret_env_names,
            vec!["API_TOKEN".to_string()]
        );
        assert_eq!(
            handoff.diagnostics.status[0].missing_source_env_names,
            vec!["SOURCE_API_TOKEN".to_string()]
        );
        assert!(serialized.contains("SOURCE_API_TOKEN"));
        assert!(!serialized.contains("secret-value"));
        assert!(!serialized.contains("super-secret"));
    }

    #[test]
    fn plan_resolver_reports_missing_source_env_names_without_values() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "API_TOKEN".to_string(),
            required: true,
            source_env_names: vec![
                "PRIMARY_API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
            ],
            refresh: None,
        }]);
        let resolver =
            SecretEnvResolver::new(vec![SecretEnvValueProvider::new("test-env", |_| {
                None::<String>
            })]);

        let error = resolver
            .resolve_plan(&plan, "missing required secret env")
            .expect_err("all source env fallbacks are missing");

        assert_eq!(error.missing_secret_env, vec!["API_TOKEN".to_string()]);
        assert_eq!(
            error.status[0].missing_source_env_names,
            vec![
                "PRIMARY_API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
            ]
        );
        let diagnostics = plan
            .diagnose(error.status.clone())
            .expect_err("required mapped secret is missing")
            .diagnostics;
        assert_eq!(
            diagnostics.status[0].source_env_names,
            vec![
                "PRIMARY_API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
            ]
        );
        assert_eq!(
            diagnostics.status[0].missing_source_env_names,
            vec![
                "PRIMARY_API_TOKEN".to_string(),
                "FALLBACK_API_TOKEN".to_string(),
            ]
        );
        let serialized = serde_json::to_string(&diagnostics).expect("diagnostics json");
        assert!(serialized.contains("PRIMARY_API_TOKEN"));
        assert!(serialized.contains("FALLBACK_API_TOKEN"));
        assert!(!serialized.contains("secret-value"));
    }

    #[test]
    fn diagnostics_fail_for_missing_required_secret_env() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "PROVIDER_TOKEN".to_string(),
            required: true,
            source_env_names: Vec::new(),
            refresh: None,
        }]);

        let error = plan
            .diagnose([secret_env_status(
                "PROVIDER_TOKEN".to_string(),
                false,
                "missing",
            )])
            .expect_err("required secret env is missing");

        assert_eq!(
            error.missing_required_secret_env,
            vec!["PROVIDER_TOKEN".to_string()]
        );
        assert_eq!(
            error.diagnostics.passthrough_secret_env_names,
            vec!["PROVIDER_TOKEN".to_string()]
        );
    }

    #[test]
    fn diagnostics_allow_missing_optional_secret_env() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "OPTIONAL_PROVIDER_TOKEN".to_string(),
            required: false,
            source_env_names: Vec::new(),
            refresh: None,
        }]);

        let diagnostics = plan
            .diagnose([])
            .expect("optional secret env can be absent");

        assert_eq!(
            diagnostics.passthrough_secret_env_names,
            vec!["OPTIONAL_PROVIDER_TOKEN".to_string()]
        );
        assert_eq!(
            diagnostics.status,
            vec![SecretEnvDiagnosticStatus {
                name: "OPTIONAL_PROVIDER_TOKEN".to_string(),
                required: false,
                configured: false,
                source: "missing".to_string(),
                source_env_names: vec!["OPTIONAL_PROVIDER_TOKEN".to_string()],
                missing_source_env_names: Vec::new(),
                refresh: None,
            }]
        );
        assert!(diagnostics.inherited_env.is_empty());
    }

    #[test]
    fn diagnostics_reject_undeclared_inherited_secret_env() {
        let plan = SecretEnvPlan::default();

        let error = plan
            .diagnose_inherited_env(
                &BTreeMap::from([(
                    "UNDECLARED_API_TOKEN".to_string(),
                    "secret-value".to_string(),
                )]),
                "runner.env",
            )
            .expect_err("sensitive inherited env must be declared");

        assert_eq!(
            error.undeclared_inherited_secret_env,
            vec!["UNDECLARED_API_TOKEN".to_string()]
        );
        assert_eq!(
            error.diagnostics.inherited_env,
            vec![SecretEnvInheritedEnvStatus {
                name: "UNDECLARED_API_TOKEN".to_string(),
                declared: false,
                source: "runner.env".to_string(),
            }]
        );
        assert!(!serde_json::to_string(&error)
            .expect("error json")
            .contains("secret-value"));
    }

    #[test]
    fn diagnostics_allow_declared_or_allowlisted_inherited_secret_env() {
        let mut plan = SecretEnvPlan::from_secret_env_names(["DECLARED_API_TOKEN".to_string()]);
        plan.allow_inherited_env_names(["HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string()]);

        let diagnostics = plan
            .diagnose_inherited_env(
                &BTreeMap::from([
                    ("DECLARED_API_TOKEN".to_string(), "secret-value".to_string()),
                    (
                        "HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string(),
                        "DECLARED_API_TOKEN".to_string(),
                    ),
                ]),
                "request.env",
            )
            .expect("declared sensitive inherited env is allowed");

        assert_eq!(diagnostics.inherited_env.len(), 2);
        assert!(diagnostics
            .inherited_env
            .iter()
            .all(|status| status.declared));
    }

    #[test]
    fn diagnostics_are_redacted_and_expose_passthrough_names_only() {
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
            required: true,
            source_env_names: Vec::new(),
            refresh: None,
        }]);

        let diagnostics = plan
            .diagnose([secret_env_status(
                "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
                true,
                "test-source",
            )])
            .expect("required secret env is configured");
        let serialized = serde_json::to_string(&diagnostics).expect("diagnostics json");

        assert_eq!(
            diagnostics.passthrough_secret_env_names,
            vec!["FIXTURE_PROVIDER_ACCESS_TOKEN".to_string()]
        );
        assert!(!serialized.contains("secret-value"));
        assert!(!serialized.contains("access-token-value"));
    }

    #[test]
    fn diagnostics_preserve_provider_refresh_metadata() {
        let refresh = SecretEnvRefreshHint {
            provider: "fixture-provider".to_string(),
            metadata: BTreeMap::from([
                ("credential_kind".to_string(), "oauth".to_string()),
                (
                    "refresh_env".to_string(),
                    "FIXTURE_PROVIDER_REFRESH_TOKEN".to_string(),
                ),
            ]),
        };
        let plan = SecretEnvPlan::from_requirements([SecretEnvRequirement {
            name: "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
            required: true,
            source_env_names: Vec::new(),
            refresh: Some(refresh.clone()),
        }]);

        let diagnostics = plan
            .diagnose([secret_env_status(
                "FIXTURE_PROVIDER_ACCESS_TOKEN".to_string(),
                false,
                "missing",
            )])
            .expect_err("missing required secret env")
            .diagnostics;

        assert_eq!(diagnostics.status[0].refresh, Some(refresh));
    }

    #[test]
    fn materialize_request_builds_plan_from_declarations_and_source_maps() {
        let output = SecretEnvPlan::materialize_request(SecretEnvPlanMaterializeRequest {
            public_env: BTreeMap::from([("PUBLIC_FLAG".to_string(), "1".to_string())]),
            secret_env_names: vec!["DIRECT_SECRET".to_string()],
            source_env_map: BTreeMap::from([(
                "TARGET_SECRET".to_string(),
                vec![
                    "PRIMARY_TARGET_SECRET".to_string(),
                    "FALLBACK_TARGET_SECRET".to_string(),
                ],
            )]),
            env_name_mapping: BTreeMap::from([(
                "source_refs".to_string(),
                vec!["MAPPED_SECRET".to_string()],
            )]),
            inherited_allowed_env_names: vec!["HOMEBOY_AGENT_RUNTIME_SECRET_ENV".to_string()],
            inherited_env_names: vec!["TARGET_SECRET".to_string()],
            ..SecretEnvPlanMaterializeRequest::default()
        })
        .expect("materialized secret env plan");

        assert_eq!(output.schema, SECRET_ENV_PLAN_MATERIALIZATION_SCHEMA);
        assert!(output.valid);
        assert_eq!(output.plan.schema, SECRET_ENV_PLAN_SCHEMA);
        assert_eq!(
            output.plan.secret_env_names(),
            vec![
                "DIRECT_SECRET".to_string(),
                "FALLBACK_TARGET_SECRET".to_string(),
                "MAPPED_SECRET".to_string(),
                "PRIMARY_TARGET_SECRET".to_string(),
                "TARGET_SECRET".to_string(),
            ]
        );
        assert_eq!(
            output.diagnostics.passthrough_secret_env_names,
            vec![
                "DIRECT_SECRET".to_string(),
                "MAPPED_SECRET".to_string(),
                "TARGET_SECRET".to_string(),
            ]
        );
        assert_eq!(output.diagnostics.status[2].name, "TARGET_SECRET");
        assert_eq!(
            output.diagnostics.status[2].source_env_names,
            vec![
                "PRIMARY_TARGET_SECRET".to_string(),
                "FALLBACK_TARGET_SECRET".to_string(),
            ]
        );
        assert!(!serde_json::to_string(&output)
            .expect("materialization json")
            .contains("secret-value"));
    }

    #[test]
    fn materialize_request_rejects_undeclared_inherited_secret_env_names() {
        let error = SecretEnvPlan::materialize_request(SecretEnvPlanMaterializeRequest {
            inherited_env_names: vec!["UNDECLARED_API_TOKEN".to_string()],
            ..SecretEnvPlanMaterializeRequest::default()
        })
        .expect_err("undeclared sensitive inherited env is rejected");

        assert_eq!(
            error.undeclared_inherited_secret_env,
            vec!["UNDECLARED_API_TOKEN".to_string()]
        );
        assert!(!serde_json::to_string(&error)
            .expect("error json")
            .contains("secret-value"));
    }
}
