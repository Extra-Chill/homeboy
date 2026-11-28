use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use serde_json::{json, Value};

#[test]
fn runner_set_help_explains_env_merge_delete_and_replace() {
    let context = HermeticTestContext::new();
    let help = runner_command(&context, &["set", "--help"]);
    assert!(help.contains("Objects merge by key"), "{help}");
    assert!(help.contains("for nested arrays"), "{help}");
    assert!(help.contains(r#"{"env":{"OLD_MODE":null}}"#), "{help}");
    assert!(help.contains("--replace env replaces"), "{help}");
}

#[test]
fn runner_set_merges_deletes_replaces_and_clears_env() {
    let context = HermeticTestContext::new();
    std::fs::write(
        context.runner_dir().join("dev.json"),
        r#"{"kind":"local","env":{"KEEP":"before","OLD_ENTRY":"old"}}"#,
    )
    .expect("write local runner");
    let servers = context.config_dir().join("servers");
    std::fs::create_dir_all(&servers).expect("create server config directory");
    std::fs::write(
        servers.join("ssh-runner.json"),
        r#"{"host":"lab.example.test","user":"runner","runner":{"env":{"KEEP":"before","OLD_ENTRY":"old"}}}"#,
    )
    .expect("write SSH runner");

    for id in ["dev", "ssh-runner"] {
        runner_command(
            &context,
            &[
                "set",
                id,
                "--json",
                r#"{"env":{"OLD_ENTRY":null,"ADDED":"new"}}"#,
            ],
        );
        assert_eq!(
            runner_env(&context, id),
            json!({"KEEP":"before","ADDED":"new"})
        );

        runner_command(
            &context,
            &[
                "set",
                id,
                "--replace",
                "env",
                "--json",
                r#"{"env":{"Only_Entry":"value"}}"#,
            ],
        );
        assert_eq!(runner_env(&context, id), json!({"Only_Entry":"value"}));

        runner_command(
            &context,
            &["set", id, "--json", r#"{"env":{"Only_Entry":null}}"#],
        );
        assert_eq!(runner_env(&context, id), json!({}));

        runner_command(
            &context,
            &[
                "set",
                id,
                "--json",
                r#"{"env":{"ABSENT":null,"RESTORED":"value"}}"#,
            ],
        );
        assert_eq!(runner_env(&context, id), json!({"RESTORED":"value"}));

        runner_command(
            &context,
            &["set", id, "--replace", "env", "--json", r#"{"env":{}}"#],
        );
        assert_eq!(runner_env(&context, id), json!({}));
    }
}

#[test]
fn runner_set_honors_env_replacement_in_bulk() {
    let context = HermeticTestContext::new();
    std::fs::write(
        context.runner_dir().join("dev.json"),
        r#"{"kind":"local","env":{"OLD_ENTRY":"old"}}"#,
    )
    .expect("write local runner");

    runner_command(
        &context,
        &[
            "set",
            "--replace",
            "env",
            "--json",
            r#"[{"id":"dev","env":{"NEW_ENTRY":"new"}}]"#,
        ],
    );
    assert_eq!(runner_env(&context, "dev"), json!({"NEW_ENTRY":"new"}));
}

fn runner_command(context: &HermeticTestContext, args: &[&str]) -> String {
    let output = context
        .command(TestBinary::HomeboyFixture)
        .arg("runner")
        .args(args)
        .output()
        .expect("run homeboy runner");
    assert!(
        output.status.success(),
        "runner {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    String::from_utf8(output.stdout).expect("UTF-8 output")
}

fn runner_env(context: &HermeticTestContext, id: &str) -> Value {
    let output = runner_command(context, &["show", id]);
    let envelope: Value = serde_json::from_str(&output).expect("runner show JSON");
    envelope["data"]["entity"]["env"].clone()
}
