//! Operator defaults must never silently choose between remote runners.
use homeboy::runner::runners::{self as runner, Runner};
use homeboy_runner_contract::RunnerKind;

pub(super) fn resolve(id: Option<String>) -> homeboy::core::Result<String> {
    if let Some(id) = id {
        return Ok(id);
    }
    select_remote(remote_ids(&runner::list()?))
}

pub(super) fn remote_ids(runners: &[Runner]) -> Vec<String> {
    runners
        .iter()
        .filter(|runner| runner.kind != RunnerKind::Local)
        .map(|runner| runner.id.clone())
        .collect()
}

fn select_remote(mut ids: Vec<String>) -> homeboy::core::Result<String> {
    ids.sort();
    match ids.as_slice() {
        [id] => Ok(id.clone()),
        _ => {
            Err(homeboy::core::Error::validation_invalid_argument(
                "runner_id",
                if ids.is_empty() {
                    "No non-local runners are configured. Pass a runner ID explicitly (use local for this machine), or configure a runner with `homeboy runner add`.".to_string()
                } else {
                    format!("Several non-local runners are configured: {}. Pass a runner ID explicitly.", ids.join(", "))
                },
                None,
                None,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_runners_do_not_compete_with_remote_default() {
        let local: Runner = serde_json::from_value(serde_json::json!({
            "id": "local", "kind": "local"
        }))
        .unwrap();
        let mut remote = local.clone();
        remote.id = "lab".into();
        remote.kind = RunnerKind::Ssh;
        assert!(remote_ids(&[local.clone()]).is_empty());
        let ids = remote_ids(&[local, remote]);
        assert_eq!(select_remote(ids).unwrap(), "lab");
    }

    #[test]
    fn selects_only_remote_and_lists_ambiguous_ids() {
        assert_eq!(select_remote(vec!["lab".into()]).unwrap(), "lab");
        let error = select_remote(vec!["zebra".into(), "alpha".into()]).unwrap_err();
        assert!(error.to_string().contains("alpha, zebra"));
        assert!(error.to_string().contains("Pass a runner ID"));
    }

    #[test]
    fn no_remote_requires_explicit_selection() {
        let error = select_remote(vec![]).unwrap_err();
        assert!(error.to_string().contains("No non-local runners"));
        assert!(error.to_string().contains("local"));
    }

    #[test]
    fn explicit_ids_bypass_default_selection() {
        for id in ["local", "localhost", "self", "custom"] {
            assert_eq!(resolve(Some(id.into())).unwrap(), id);
        }
    }
}
