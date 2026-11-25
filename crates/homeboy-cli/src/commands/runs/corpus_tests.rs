//! Integration tests for the gh-actions branch of `runs import`.

#![cfg(test)]

use homeboy::test_support::with_isolated_home;

use super::bundle::import_runs;
use super::bundle::RunsImportArgs;

#[test]
fn import_from_gh_actions_requires_gh_specific_arguments() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let store =
            homeboy::core::observation::ObservationStore::open_initialized().expect("store");
        let cases = [
            // Without --component, --repo, --workflow/--run-id, --artifact-glob.
            RunsImportArgs {
                from_gh_actions: true,
                ..RunsImportArgs::default()
            },
            // Everything except --workflow/--run-id.
            RunsImportArgs {
                from_gh_actions: true,
                component_id: Some("wp-site-generator".into()),
                repo: Some("example-org/wp-site-generator".into()),
                artifact_glob: Some("php-transformer-iterator-transcript-*".into()),
                ..RunsImportArgs::default()
            },
        ];

        for args in cases {
            let err = import_runs(&store, args)
                .err()
                .expect("must fail without required gh-actions args");
            assert_eq!(err.code.as_str(), "validation.missing_argument");
        }
    });
}
