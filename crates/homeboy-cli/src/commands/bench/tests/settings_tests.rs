use super::*;

fn setting_args(key: &str) -> SettingArgs {
    SettingArgs {
        settings_json_file: Vec::new(),
        setting: vec![(key.to_string(), "1".to_string())],
        setting_json: Vec::new(),
    }
}

#[test]
fn setting_keys_declared_by_extension_or_rig_do_not_warn() {
    let ctx = ctx_with_accepted_setting_keys(&["workflow_bench_env"]);
    let rig_accepted = vec!["fixture_matrix_namespace".to_string()];

    for key in ["workflow_bench_env.CORPUS", "fixture_matrix_namespace"] {
        assert!(
            unknown_setting_keys_warning(&ctx, &setting_args(key), &rig_accepted).is_none(),
            "{key} is declared and must not warn"
        );
    }
}

#[test]
fn unknown_setting_key_warns_with_accepted_keys() {
    let ctx = ctx_with_accepted_setting_keys(&["workflow_bench_env", "iterations"]);
    let rig_accepted = vec!["fixture_matrix_namespace".to_string()];

    // `bench_env` is a typo for the declared `workflow_bench_env`.
    let warning =
        unknown_setting_keys_warning(&ctx, &setting_args("bench_env.CORPUS"), &rig_accepted)
            .expect("unknown key should warn");
    for expected in [
        "bench_env",
        "workflow_bench_env",
        "fixture_matrix_namespace",
        "extension 'rust'",
    ] {
        assert!(warning.contains(expected), "missing {expected}: {warning}");
    }
}
