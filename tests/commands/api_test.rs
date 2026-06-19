use super::{require_apply_for_mutation, ApiArgs, ApiCommand};

#[test]
fn api_mutating_commands_require_apply() {
    for command in [
        ApiCommand::Post {
            endpoint: "/wp/v2/posts".to_string(),
            apply: false,
            body: None,
            form: Vec::new(),
        },
        ApiCommand::Put {
            endpoint: "/wp/v2/posts/1".to_string(),
            apply: false,
            body: None,
            form: Vec::new(),
        },
        ApiCommand::Patch {
            endpoint: "/wp/v2/posts/1".to_string(),
            apply: false,
            body: None,
            form: Vec::new(),
        },
        ApiCommand::Delete {
            endpoint: "/wp/v2/posts/1".to_string(),
            apply: false,
        },
    ] {
        let err = require_apply_for_mutation(&ApiArgs {
            project_id: "site".to_string(),
            command,
        })
        .expect_err("mutating API command should require --apply");

        assert!(err.message.contains("requires explicit --apply"));
        assert!(err.message.contains("Suggested command: homeboy api site"));
    }
}

#[test]
fn api_get_and_applied_mutations_pass_apply_guard() {
    require_apply_for_mutation(&ApiArgs {
        project_id: "site".to_string(),
        command: ApiCommand::Get {
            endpoint: "/wp/v2/posts".to_string(),
        },
    })
    .expect("GET should not require --apply");

    require_apply_for_mutation(&ApiArgs {
        project_id: "site".to_string(),
        command: ApiCommand::Post {
            endpoint: "/wp/v2/posts".to_string(),
            apply: true,
            body: None,
            form: Vec::new(),
        },
    })
    .expect("applied mutation should pass guard");
}
