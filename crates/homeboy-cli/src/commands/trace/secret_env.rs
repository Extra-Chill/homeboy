//! Trace secret-environment resolution and application.
//!
//! Resolves declared secret env names into concrete values (once per run),
//! hydrates a child exec environment, and folds resolved secrets into trace
//! command args so matrix/repeat/compare layers can reuse a single resolution.
//! Split out of the trace command root to keep it under its structural line
//! threshold; the parent re-exports the externally consumed helpers so sibling
//! modules keep their stable `super::` import paths.

use super::{load_rig_context, resolve_component_id, TraceArgs};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTraceSecretEnv {
    pub(super) env: Vec<(String, String)>,
}

pub(super) fn hydrate_trace_secret_env(
    names: &[String],
    project_id: Option<&str>,
    env: &mut Vec<(String, String)>,
) -> homeboy::core::Result<serde_json::Value> {
    if names.iter().all(|name| name.trim().is_empty()) {
        return Ok(homeboy::core::trace_secrets::empty_status());
    }

    let (resolved, statuses) = homeboy::core::trace_secrets::resolve_secret_env(names, project_id)?;
    env.extend(resolved);
    Ok(homeboy::core::trace_secrets::status_metadata(statuses))
}

pub(crate) fn resolve_trace_secret_env_once(
    names: &[String],
    project_id: Option<&str>,
) -> homeboy::core::Result<Option<ResolvedTraceSecretEnv>> {
    if names.iter().all(|name| name.trim().is_empty()) {
        return Ok(None);
    }

    let (env, _statuses) = homeboy::core::trace_secrets::resolve_secret_env(names, project_id)?;
    Ok(Some(ResolvedTraceSecretEnv { env }))
}

pub(crate) fn apply_resolved_trace_secret_env(
    args: &mut TraceArgs,
    resolved: Option<&ResolvedTraceSecretEnv>,
) {
    apply_resolved_trace_secret_env_to_fields(&mut args.secret_env, &mut args.matrix_env, resolved);
}

pub(super) fn apply_resolved_trace_secret_env_to_fields(
    secret_env: &mut Vec<String>,
    matrix_env: &mut Vec<(String, String)>,
    resolved: Option<&ResolvedTraceSecretEnv>,
) {
    if let Some(resolved) = resolved {
        matrix_env.extend(resolved.env.clone());
        secret_env.clear();
    }
}

pub(crate) fn trace_secret_env_project_id_for_args(
    args: &TraceArgs,
) -> homeboy::core::Result<String> {
    let rig_context = load_rig_context(args.rig.as_deref())?;
    resolve_component_id(
        &args.comp,
        rig_context.as_ref().map(|context| &context.rig_spec),
    )
}

#[cfg(test)]
mod secret_env_tests {
    use super::*;

    #[test]
    fn resolved_trace_secret_env_moves_values_to_child_matrix_env() {
        let mut secret_env = vec!["STRIPE_SECRET_KEY".to_string()];
        let mut matrix_env = vec![("EXISTING".to_string(), "1".to_string())];
        let resolved = ResolvedTraceSecretEnv {
            env: vec![(
                "STRIPE_SECRET_KEY".to_string(),
                "redacted-test-value".to_string(),
            )],
        };

        apply_resolved_trace_secret_env_to_fields(
            &mut secret_env,
            &mut matrix_env,
            Some(&resolved),
        );

        assert!(secret_env.is_empty());
        assert_eq!(
            matrix_env,
            vec![
                ("EXISTING".to_string(), "1".to_string()),
                (
                    "STRIPE_SECRET_KEY".to_string(),
                    "redacted-test-value".to_string()
                )
            ]
        );
    }

    #[test]
    fn unresolved_trace_secret_env_leaves_child_args_unchanged() {
        let mut secret_env = vec!["STRIPE_SECRET_KEY".to_string()];
        let mut matrix_env = Vec::new();

        apply_resolved_trace_secret_env_to_fields(&mut secret_env, &mut matrix_env, None);

        assert_eq!(secret_env, vec!["STRIPE_SECRET_KEY".to_string()]);
        assert!(matrix_env.is_empty());
    }
}
