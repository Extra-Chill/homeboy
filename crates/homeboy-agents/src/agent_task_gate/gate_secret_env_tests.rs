use super::*;

const WP_NAMES: [&str; 4] = [
    "WP_CODEBOX_DB_HOST",
    "WP_CODEBOX_DB_PASSWORD",
    "WP_CODEBOX_DB_PORT",
    "WP_CODEBOX_DB_USER",
];

/// A monorepo root without test secrets, plus `plugins/events` whose test
/// extension declares the WP_CODEBOX_DB_* names (#19187).
fn monorepo_fixture(home: &Path) -> tempfile::TempDir {
    let extension = home.join(".config/homeboy/extensions/secret-wp-fixture");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::write(
        extension.join("secret-wp-fixture.json"),
        serde_json::json!({
            "name": "Secret WP fixture",
            "version": "1.0.0",
            "test": {
                "extension_script": "test.sh",
                "secret_env": WP_NAMES.iter().map(|name| (*name, *name)).collect::<std::collections::BTreeMap<_, _>>(),
            }
        })
        .to_string(),
    )
    .unwrap();
    homeboy_test_support::write_executable(extension.join("test.sh"), "#!/bin/sh\nexit 0\n", 0o755)
        .unwrap();

    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("homeboy.json"),
        r#"{"id":"mono-root-19187"}"#,
    )
    .unwrap();
    let events = root.path().join("plugins/mono-events-19187");
    std::fs::create_dir_all(&events).unwrap();
    std::fs::write(
        events.join("homeboy.json"),
        r#"{"id":"mono-events-19187","extensions":{"secret-wp-fixture":{}}}"#,
    )
    .unwrap();
    root
}

fn expected() -> Vec<String> {
    WP_NAMES.iter().map(|name| name.to_string()).collect()
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

#[test]
fn argv_form_carries_the_tested_subcomponent_names() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = monorepo_fixture(home.path());
        let argv = args(&["homeboy", "review", "test", "mono-events-19187"]);
        assert!(
            gate_test_secret_env_names(Some("mono-root-19187"), root.path(), []).is_empty(),
            "the admitted root declares no test secrets"
        );
        assert_eq!(
            gate_test_secret_env_names(Some("mono-root-19187"), root.path(), [argv.as_slice()]),
            expected()
        );
        let shell = crate::agent_task_gate::legacy_gate_argv(
            "homeboy --placement local test mono-events-19187 --skip-lint",
        );
        assert_eq!(
            gate_test_secret_env_names(Some("mono-root-19187"), root.path(), [shell.as_slice()]),
            expected()
        );
    });
}

#[test]
fn cd_subdir_form_carries_the_tested_subcomponent_names() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = monorepo_fixture(home.path());
        let shell = crate::agent_task_gate::legacy_gate_argv(
            "( cd plugins/mono-events-19187 && homeboy review test )",
        );
        assert_eq!(
            gate_test_secret_env_names(Some("mono-root-19187"), root.path(), [shell.as_slice()]),
            expected()
        );
    });
}

#[test]
fn non_test_and_out_of_tree_commands_request_no_names() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = monorepo_fixture(home.path());
        for script in [
            "( cd plugins/mono-events-19187 && homeboy lint )",
            "( cd plugins/mono-events-19187 ) && homeboy review test",
            "cd ../.. && homeboy review test",
            "echo homeboy review test mono-events-19187",
        ] {
            let shell = crate::agent_task_gate::legacy_gate_argv(script);
            assert!(
                gate_test_secret_env_names(
                    Some("mono-root-19187"),
                    root.path(),
                    [shell.as_slice()]
                )
                .is_empty(),
                "{script}"
            );
        }
    });
}

#[test]
fn shell_targets_follow_cd_and_subshell_scope() {
    let cwd = Path::new("/work");
    let targets = gate_test_targets(
        cwd,
        &crate::agent_task_gate::legacy_gate_argv(
            "cargo build && ( cd 'plugins/a' && FOO=1 homeboy review test ) ; homeboy test b; homeboy review test --path plugins/c",
        ),
    );
    assert_eq!(
        targets,
        vec![
            GateTestTarget::Directory(PathBuf::from("/work/plugins/a")),
            GateTestTarget::Id("b".to_string()),
            GateTestTarget::Directory(PathBuf::from("/work/plugins/c")),
        ]
    );
}
