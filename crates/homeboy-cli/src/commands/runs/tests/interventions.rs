use super::*;
use clap::Parser;

#[test]
fn runs_interventions_audit_cli_mutations_but_not_noops_or_automation() {
    with_isolated_home(|_| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let _actor = homeboy_core::test_support::EnvVarGuard::set("HOMEBOY_ACTOR", "soak-operator");
        let invoke = |argv: &[&str]| {
            let cli = crate::cli_surface::Cli::try_parse_from(argv).unwrap();
            let crate::cli_surface::Commands::Runs(args) = cli.command else {
                panic!("runs")
            };
            crate::commands::runs::run(args)
        };
        let query = || {
            let (output, code) =
                invoke(&["homeboy", "runs", "interventions", "--since", "1h"]).unwrap();
            assert_eq!(code, 0);
            serde_json::to_value(output).unwrap()
        };
        assert_eq!(query(), serde_json::json!([]));
        let store = test_store();
        for id in ["cancel-me", "reconcile-me", "automatic"] {
            store.upsert_imported_run(&dead_owned_run(id)).unwrap();
        }
        // The daemon uses this same service, not the CLI adapter.
        runs_service::reconcile_orphaned_running_runs(
            &store,
            vec![dead_owned_run("automatic")],
            false,
            |_| false,
        )
        .unwrap();
        assert_eq!(query(), serde_json::json!([]));
        for argv in [
            vec![
                "homeboy",
                "runs",
                "cancel",
                "cancel-me",
                "--reason",
                "soak cleanup",
            ],
            vec!["homeboy", "runs", "reconcile", "--reason", "soak cleanup"],
            vec!["homeboy", "runs", "reconcile", "--reason", "no-op"],
        ] {
            assert_eq!(invoke(&argv).unwrap().1, 0);
        }
        assert!(invoke(&[
            "homeboy",
            "runs",
            "cancel",
            "cancel-me",
            "--reason",
            "already done"
        ])
        .is_err());
        let events = query();
        let events = events.as_array().unwrap();
        assert_eq!(events.len(), 2);
        for (event, command) in events.iter().zip(["runs reconcile", "runs cancel"]) {
            assert_eq!(event["actor"], "soak-operator");
            assert_eq!(event["command"], command);
            assert_eq!(event["reason"], "soak cleanup");
        }
    });
}
