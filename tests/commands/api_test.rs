use super::{require_apply_for_mutation, ApiArgs, ApiCommand};

#[test]
fn api_mutating_commands_require_apply() {
    for command in [
        ApiCommand::Post {
            project_id: "site".to_string(),
            endpoint: "/wp/v2/posts".to_string(),
            apply: false,
            body: None,
            form: Vec::new(),
        },
        ApiCommand::Put {
            project_id: "site".to_string(),
            endpoint: "/wp/v2/posts/1".to_string(),
            apply: false,
            body: None,
            form: Vec::new(),
        },
        ApiCommand::Patch {
            project_id: "site".to_string(),
            endpoint: "/wp/v2/posts/1".to_string(),
            apply: false,
            body: None,
            form: Vec::new(),
        },
        ApiCommand::Delete {
            project_id: "site".to_string(),
            endpoint: "/wp/v2/posts/1".to_string(),
            apply: false,
        },
    ] {
        let err = require_apply_for_mutation(&ApiArgs { command })
            .expect_err("mutating API command should require --apply");

        assert!(err.message.contains("requires explicit --apply"));
        assert!(err.message.contains("Suggested command: homeboy api "));
        assert!(err.message.contains(" site "));
    }
}

#[test]
fn api_get_and_applied_mutations_pass_apply_guard() {
    require_apply_for_mutation(&ApiArgs {
        command: ApiCommand::Get {
            project_id: "site".to_string(),
            endpoint: "/wp/v2/posts".to_string(),
        },
    })
    .expect("GET should not require --apply");

    require_apply_for_mutation(&ApiArgs {
        command: ApiCommand::Post {
            project_id: "site".to_string(),
            endpoint: "/wp/v2/posts".to_string(),
            apply: true,
            body: None,
            form: Vec::new(),
        },
    })
    .expect("applied mutation should pass guard");
}
