use crate::error::{Error, Result};
use crate::secret_env_plan::{resolve_secret_env_names, SecretEnvValueProvider};

/// Resolve secret identities for a local child from Homeboy's authorized
/// controller sources. Values are returned only for direct child injection;
/// diagnostics contain names and source guidance, never values.
pub fn resolve_local_required(
    names: impl IntoIterator<Item = String>,
    field: &str,
    workload: &str,
) -> Result<Vec<(String, String)>> {
    resolve_secret_env_names(
        names,
        vec![
            SecretEnvValueProvider::new("process-env", |name| std::env::var(name).ok()),
            SecretEnvValueProvider::new("homeboy-config", |name| {
                crate::agent_task_secret_provider::resolve_agent_task_secret_env(&[
                    name.to_string(),
                ])
                .into_iter()
                .next()
                .map(|(_, value)| value)
            }),
        ],
        &format!("missing required {workload} secret env"),
    )
    .map(|resolution| resolution.env)
    .map_err(|error| {
        Error::validation_invalid_argument(
            field,
            error.message,
            None,
            Some(vec![
                "Export each declared variable in the controller environment, or configure an authorized Homeboy secret mapping with `homeboy agent-task auth map-env` / `homeboy agent-task auth set-keychain`.".to_string(),
                "Homeboy resolves declared local child secrets from process environment first, then configured secret mappings; it never reads component settings as secret values.".to_string(),
            ]),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_declared_local_secret_without_exposing_value_in_errors() {
        let name = format!("HOMEBOY_LOCAL_CHILD_SECRET_{}", std::process::id());
        std::env::set_var(&name, "fixture-local-secret");

        let resolved = resolve_local_required([name.clone()], "test.secret_env", "test child")
            .expect("declared secret resolves");

        assert_eq!(resolved, vec![(name.clone(), "fixture-local-secret".to_string())]);
        std::env::remove_var(&name);

        let error = resolve_local_required([name.clone()], "test.secret_env", "test child")
            .expect_err("missing secret fails before child execution");
        assert!(error.message.contains(&name));
        assert!(!error.to_string().contains("fixture-local-secret"));
    }
}
