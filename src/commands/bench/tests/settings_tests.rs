use super::*;

#[test]
fn unknown_setting_key_warns_before_run() {
    let ctx = ctx_with_accepted_setting_keys(&["workflow_bench_env", "iterations"]);
    let setting_args = SettingArgs {
        // `bench_env` is a typo for the declared `workflow_bench_env`.
        setting: vec![("bench_env.CORPUS".to_string(), "1000".to_string())],
        setting_json: Vec::new(),
    };

    let warning =
        unknown_setting_keys_warning(&ctx, &setting_args, &[]).expect("unknown key should warn");
    assert!(
        warning.contains("bench_env"),
        "warning names the typo: {warning}"
    );
    assert!(
        warning.contains("workflow_bench_env"),
        "warning lists accepted settings: {warning}"
    );
    assert!(
        warning.contains("extension 'rust'"),
        "warning names the resolved extension: {warning}"
    );
}

#[test]
fn declared_setting_key_does_not_warn() {
    let ctx = ctx_with_accepted_setting_keys(&["workflow_bench_env"]);
    let setting_args = SettingArgs {
        setting: vec![("workflow_bench_env.CORPUS".to_string(), "1000".to_string())],
        setting_json: Vec::new(),
    };

    assert!(unknown_setting_keys_warning(&ctx, &setting_args, &[]).is_none());
}

#[test]
fn declared_rig_setting_key_does_not_warn_as_extension_unknown() {
    let ctx = ctx_with_accepted_setting_keys(&["workflow_bench_env"]);
    let setting_args = SettingArgs {
        setting: vec![(
            "fixture_matrix_namespace".to_string(),
            "nightly".to_string(),
        )],
        setting_json: Vec::new(),
    };
    let rig_accepted = vec!["fixture_matrix_namespace".to_string()];

    assert!(unknown_setting_keys_warning(&ctx, &setting_args, &rig_accepted).is_none());
}

#[test]
fn unknown_setting_still_warns_when_not_declared_by_extension_or_rig() {
    let ctx = ctx_with_accepted_setting_keys(&["workflow_bench_env"]);
    let setting_args = SettingArgs {
        setting: vec![(
            "unexpected_fixture_input".to_string(),
            "nightly".to_string(),
        )],
        setting_json: Vec::new(),
    };
    let rig_accepted = vec!["fixture_matrix_namespace".to_string()];

    let warning = unknown_setting_keys_warning(&ctx, &setting_args, &rig_accepted)
        .expect("unknown key should warn");
    assert!(warning.contains("unexpected_fixture_input"));
    assert!(warning.contains("fixture_matrix_namespace"));
}
