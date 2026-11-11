//! Provider-neutral execution environment and exact placement requirements.
use serde::{Deserialize, Serialize};

pub const RUNNER_ENVIRONMENT_SCHEMA: &str = "homeboy/runner-environment/v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerPlatform {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerArch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerExecutionKind {
    #[default]
    Host,
    Vm,
    Emulator,
    Simulator,
    PhysicalDevice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerEnvironment {
    pub schema: String,
    pub platform: RunnerPlatform,
    pub arch: RunnerArch,
    #[serde(default)]
    pub execution_kind: RunnerExecutionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
}

impl std::fmt::Display for RunnerEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = serde_json::to_value(self).map_err(|_| std::fmt::Error)?;
        write!(
            f,
            "{} / {} / {}",
            value["platform"].as_str().unwrap_or("unknown"),
            value["arch"].as_str().unwrap_or("unknown"),
            value["execution_kind"].as_str().unwrap_or("unknown")
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerEnvironmentRequirement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<RunnerPlatform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<RunnerArch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_kind: Option<RunnerExecutionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
}

impl RunnerEnvironmentRequirement {
    /// Kinds match exactly: no implicit fidelity upgrade or downgrade is safe.
    pub fn matches(&self, offered: &RunnerEnvironment) -> bool {
        offered.schema == RUNNER_ENVIRONMENT_SCHEMA
            && self.platform.is_none_or(|v| v == offered.platform)
            && self.arch.is_none_or(|v| v == offered.arch)
            && self
                .execution_kind
                .is_none_or(|v| v == offered.execution_kind)
            && self
                .os_version
                .as_ref()
                .is_none_or(|v| Some(v) == offered.os_version.as_ref())
    }
}

impl std::str::FromStr for RunnerEnvironmentRequirement {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut fields = serde_json::Map::new();
        for field in value.split(',') {
            let (key, value) = field.split_once('=').ok_or("expected field=value")?;
            if value.is_empty() || fields.insert(key.into(), value.into()).is_some() {
                return Err(format!("empty or duplicate environment field: {key}"));
            }
        }
        serde_json::from_value(fields.into()).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerEnvironmentOffer {
    pub runner_id: String,
    /// Missing observations must never be inferred from the controller target.
    pub environment: Option<RunnerEnvironment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerEnvironmentMismatch {
    pub requested: RunnerEnvironmentRequirement,
    pub offered: Vec<RunnerEnvironmentOffer>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_round_trip_rejects_unknown_values() {
        let value = serde_json::json!({"schema": RUNNER_ENVIRONMENT_SCHEMA,
            "platform": "windows", "arch": "x86_64", "execution_kind": "vm"});
        let environment: RunnerEnvironment = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(environment).unwrap(), value);
        for field in ["platform", "arch", "execution_kind"] {
            let mut invalid = value.clone();
            invalid[field] = "unknown".into();
            assert!(serde_json::from_value::<RunnerEnvironment>(invalid).is_err());
        }
        let mut host = value;
        host.as_object_mut().unwrap().remove("execution_kind");
        assert_eq!(
            serde_json::from_value::<RunnerEnvironment>(host)
                .unwrap()
                .execution_kind,
            RunnerExecutionKind::Host
        );
    }
}
