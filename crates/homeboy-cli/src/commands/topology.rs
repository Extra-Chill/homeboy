use clap::{Args, ValueEnum};
use homeboy_resource_topology_contract::{
    ResourceTopologyResourceKind, ResourceTopologyResourceRef, ResourceTopologySnapshot,
};

use super::{CmdResult, CommandReport};

/// Read-only inspection of declared Homeboy resource relationships.
#[derive(Args)]
pub struct TopologyArgs {
    /// Kind of the root resource to inspect.
    #[arg(value_enum)]
    kind: TopologyKind,
    /// ID of the root resource to inspect.
    id: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum TopologyKind {
    Component,
    Project,
    Server,
    Fleet,
    Runner,
}

impl From<TopologyKind> for ResourceTopologyResourceKind {
    fn from(kind: TopologyKind) -> Self {
        match kind {
            TopologyKind::Component => Self::Component,
            TopologyKind::Project => Self::Project,
            TopologyKind::Server => Self::Server,
            TopologyKind::Fleet => Self::Fleet,
            TopologyKind::Runner => Self::Runner,
        }
    }
}

pub fn run(args: TopologyArgs) -> CmdResult<CommandReport<ResourceTopologySnapshot>> {
    let runners = homeboy_lab_runner::list()?
        .into_iter()
        .map(
            |runner| homeboy::core::resource_topology::ResourceTopologyRunner {
                id: runner.id,
                server_id: runner.server_id,
            },
        )
        .collect::<Vec<_>>();
    let snapshot = homeboy::core::resource_topology::resolve(
        &[ResourceTopologyResourceRef {
            kind: args.kind.into(),
            id: args.id,
        }],
        &runners,
    )?;
    Ok((
        CommandReport {
            command: "topology.show",
            report: snapshot,
        },
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_returns_partial_topology_with_unresolved_reference_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let project_config = crate::core::paths::project_config("site").expect("project path");
            std::fs::create_dir_all(project_config.parent().expect("project directory"))
                .expect("project directory");
            std::fs::write(project_config, r#"{"server_id":"missing-server"}"#)
                .expect("legacy partial project config");
            crate::core::fleet::save(&crate::core::fleet::Fleet::new(
                "production".to_string(),
                vec!["site".to_string()],
            ))
            .expect("fleet config");

            let (output, exit_code) = run(TopologyArgs {
                kind: TopologyKind::Fleet,
                id: "production".to_string(),
            })
            .expect("topology output");

            assert_eq!(exit_code, 0);
            assert_eq!(output.report.diagnostics.len(), 1);
        });
    }
}
