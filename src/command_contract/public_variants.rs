//! Public output variant contracts.
//!
//! These describe the supported JSON output variants of public commands,
//! including how to discriminate between variants in golden fixtures and
//! which fixture (if any) anchors the wire shape. Tests across the crate
//! use [`PUBLIC_OUTPUT_VARIANT_CONTRACTS`] to enforce that every documented
//! variant either has a discriminator field or a golden fixture (or both).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicOutputVariantContract {
    pub command: &'static str,
    pub variant: &'static str,
    pub discriminator_field: Option<&'static str>,
    pub discriminator_value: Option<&'static str>,
    pub golden_fixture: Option<&'static str>,
}

pub const PUBLIC_OUTPUT_VARIANT_CONTRACTS: &[PublicOutputVariantContract] = &[
    PublicOutputVariantContract {
        command: "bench",
        variant: "single",
        discriminator_field: Some("variant"),
        discriminator_value: Some("single"),
        golden_fixture: Some("bench_contract.json"),
    },
    PublicOutputVariantContract {
        command: "bench",
        variant: "comparison",
        discriminator_field: Some("variant"),
        discriminator_value: Some("comparison"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "bench",
        variant: "comparison_summary",
        discriminator_field: Some("variant"),
        discriminator_value: Some("comparison_summary"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "bench",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: Some("bench_contract.json"),
    },
    PublicOutputVariantContract {
        command: "db",
        variant: "status",
        discriminator_field: Some("variant"),
        discriminator_value: Some("status"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "db",
        variant: "query",
        discriminator_field: Some("variant"),
        discriminator_value: Some("query"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "db",
        variant: "tunnel",
        discriminator_field: Some("variant"),
        discriminator_value: Some("tunnel"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "deploy",
        variant: "single",
        discriminator_field: Some("variant"),
        discriminator_value: Some("single"),
        golden_fixture: Some("deploy_contract.json"),
    },
    PublicOutputVariantContract {
        command: "deploy",
        variant: "multi_project",
        discriminator_field: Some("variant"),
        discriminator_value: Some("multi_project"),
        golden_fixture: Some("deploy_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "show",
        discriminator_field: Some("variant"),
        discriminator_value: Some("show"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "artifacts",
        discriminator_field: Some("variant"),
        discriminator_value: Some("artifacts"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "query",
        discriminator_field: Some("variant"),
        discriminator_value: Some("query"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "refs",
        discriminator_field: Some("variant"),
        discriminator_value: Some("refs"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "drift",
        discriminator_field: Some("variant"),
        discriminator_value: Some("drift"),
        golden_fixture: Some("runs_contract.json"),
    },
    PublicOutputVariantContract {
        command: "runs",
        variant: "loop_sync",
        discriminator_field: Some("variant"),
        discriminator_value: Some("loop_sync"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "show",
        discriminator_field: Some("variant"),
        discriminator_value: Some("show"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "up",
        discriminator_field: Some("variant"),
        discriminator_value: Some("up"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "check",
        discriminator_field: Some("variant"),
        discriminator_value: Some("check"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "down",
        discriminator_field: Some("variant"),
        discriminator_value: Some("down"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "repair",
        discriminator_field: Some("variant"),
        discriminator_value: Some("repair"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "sync",
        discriminator_field: Some("variant"),
        discriminator_value: Some("sync"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "status",
        discriminator_field: Some("variant"),
        discriminator_value: Some("status"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "install",
        discriminator_field: Some("variant"),
        discriminator_value: Some("install"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "update",
        discriminator_field: Some("variant"),
        discriminator_value: Some("update"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "sources",
        discriminator_field: Some("variant"),
        discriminator_value: Some("sources"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "rig",
        variant: "app",
        discriminator_field: Some("variant"),
        discriminator_value: Some("app"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "add",
        discriminator_field: Some("variant"),
        discriminator_value: Some("add"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "enable",
        discriminator_field: Some("variant"),
        discriminator_value: Some("enable"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "list",
        discriminator_field: Some("variant"),
        discriminator_value: Some("list"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "show",
        discriminator_field: Some("variant"),
        discriminator_value: Some("show"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "set",
        discriminator_field: Some("variant"),
        discriminator_value: Some("set"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "trust",
        discriminator_field: Some("variant"),
        discriminator_value: Some("trust"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "pair",
        discriminator_field: Some("variant"),
        discriminator_value: Some("pair"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "remove",
        discriminator_field: Some("variant"),
        discriminator_value: Some("remove"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "doctor",
        discriminator_field: Some("variant"),
        discriminator_value: Some("doctor"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "connect",
        discriminator_field: Some("variant"),
        discriminator_value: Some("connect"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "status",
        discriminator_field: Some("variant"),
        discriminator_value: Some("status"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "disconnect",
        discriminator_field: Some("variant"),
        discriminator_value: Some("disconnect"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "exec",
        discriminator_field: Some("variant"),
        discriminator_value: Some("exec"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "env",
        discriminator_field: Some("variant"),
        discriminator_value: Some("env"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "job_logs",
        discriminator_field: Some("variant"),
        discriminator_value: Some("job_logs"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "job_cancel",
        discriminator_field: Some("variant"),
        discriminator_value: Some("job_cancel"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "work",
        discriminator_field: Some("variant"),
        discriminator_value: Some("work"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "workspace_sync",
        discriminator_field: Some("variant"),
        discriminator_value: Some("workspace_sync"),
        golden_fixture: None,
    },
    PublicOutputVariantContract {
        command: "runner",
        variant: "workspace_apply",
        discriminator_field: Some("variant"),
        discriminator_value: Some("workspace_apply"),
        golden_fixture: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_variant_contracts_have_discriminators_or_fixtures() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = root.join("tests/fixtures/golden_json_contracts");

        for contract in PUBLIC_OUTPUT_VARIANT_CONTRACTS {
            assert!(
                contract.discriminator_field.is_some() || contract.golden_fixture.is_some(),
                "{}.{} needs a discriminator or golden fixture",
                contract.command,
                contract.variant
            );

            if let Some(fixture) = contract.golden_fixture {
                assert!(
                    fixtures.join(fixture).exists(),
                    "{}.{} references missing fixture {fixture}",
                    contract.command,
                    contract.variant
                );
            }
        }
    }
}
