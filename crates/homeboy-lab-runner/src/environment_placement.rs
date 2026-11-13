//! Exact, provider-neutral runner environment placement.
use homeboy_runner_contract::{
    RunnerEnvironmentMismatch, RunnerEnvironmentOffer, RunnerEnvironmentRequirement,
};

pub fn select_environment_runner(
    requested: &RunnerEnvironmentRequirement,
    offered: Vec<RunnerEnvironmentOffer>,
) -> std::result::Result<RunnerEnvironmentOffer, RunnerEnvironmentMismatch> {
    offered
        .iter()
        .find(|offer| {
            offer
                .environment
                .as_ref()
                .is_some_and(|env| requested.matches(env))
        })
        .cloned()
        .ok_or_else(|| RunnerEnvironmentMismatch {
            requested: requested.clone(),
            offered,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_runner_contract::*;
    #[test]
    fn environment_placement_matrix() {
        let requirement: RunnerEnvironmentRequirement =
            "platform=windows,arch=x86_64,execution_kind=vm"
                .parse()
                .unwrap();
        let offer = |id: &str, kind| RunnerEnvironmentOffer {
            runner_id: id.into(),
            environment: Some(RunnerEnvironment {
                schema: RUNNER_ENVIRONMENT_SCHEMA.into(),
                platform: RunnerPlatform::Windows,
                arch: RunnerArch::X86_64,
                execution_kind: kind,
                os_version: None,
            }),
        };
        for (offers, expected) in [
            (vec![offer("match", RunnerExecutionKind::Vm)], Some("match")),
            (
                vec![
                    offer("skip", RunnerExecutionKind::Host),
                    offer("match", RunnerExecutionKind::Vm),
                ],
                Some("match"),
            ),
            (vec![offer("skip", RunnerExecutionKind::Simulator)], None),
        ] {
            let result = select_environment_runner(&requirement, offers.clone());
            match expected {
                Some(id) => assert_eq!(result.unwrap().runner_id, id),
                None => assert_eq!(
                    result.unwrap_err(),
                    RunnerEnvironmentMismatch {
                        requested: requirement.clone(),
                        offered: offers
                    }
                ),
            }
        }
        let physical: RunnerEnvironmentRequirement =
            "execution_kind=physical_device".parse().unwrap();
        assert!(!physical.matches(
            offer("vm", RunnerExecutionKind::Vm)
                .environment
                .as_ref()
                .unwrap()
        ));
    }
}
