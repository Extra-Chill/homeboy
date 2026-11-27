use super::*;
use crate::cli_surface::{Cli, Commands};
use clap::Parser;

fn dispatch_with_prompt(prompt: Option<&str>) -> DispatchArgs {
    DispatchArgs {
        prompt: prompt.map(str::to_string),
        prompt_is_literal: false,
        tasks: Vec::new(),
        cwd: None,
        workspace: None,
        repo: None,
        task_url: None,
        cook: Default::default(),
        required_capabilities: Vec::new(),
        concurrency: 1,
        run_id: None,
        core: crate::commands::agent_task_dispatch::DispatchCoreArgs {
            tasks_json: None,
            client_context: None,
            attempts: Some(1),
            same_provider_retries: Some(0),
            queue_only: false,
            timeout_ms: None,
            resolved_provider_policy: None,
            deny_command: Vec::new(),
            allow_command: Vec::new(),
            command_policy_reason: None,
        },
    }
}

/// Shell-sensitive prompt content must reach the executor byte-for-byte,
/// with no interpolation and no trimming, whether the `@file` path is
/// relative or absolute (#10100).
#[test]
fn at_file_prompts_are_read_verbatim_from_relative_and_absolute_paths() {
    let cwd = std::env::current_dir().expect("current directory");
    let file = tempfile::NamedTempFile::new_in(&cwd).expect("prompt file");
    let body = "Run `cargo test` for $HOME\n\nUse \"quotes\" and 'apostrophes'.\n\n";
    std::fs::write(file.path(), body).expect("write prompt");
    let relative = file
        .path()
        .strip_prefix(&cwd)
        .expect("prompt file is in the current directory");

    for path in [relative, file.path()] {
        let mut args = dispatch_with_prompt(Some(&format!("@{}", path.display())));
        resolve_dispatch_prompt(&mut args).expect("resolve @file prompt");
        assert_eq!(args.prompt.as_deref(), Some(body), "{}", path.display());
    }
}

#[test]
fn inline_and_absent_prompts_pass_through_unchanged() {
    for prompt in [Some("fix the flaky test"), None] {
        let mut args = dispatch_with_prompt(prompt);
        resolve_dispatch_prompt(&mut args).expect("resolve prompt");
        assert_eq!(args.prompt.as_deref(), prompt);
    }
}

#[test]
fn missing_or_empty_at_file_is_reported_not_passed_through() {
    for (prompt, expected) in [
        ("@/definitely/not/a/prompt/file/10100", None),
        ("@", Some("missing file path")),
    ] {
        let mut args = dispatch_with_prompt(Some(prompt));
        let error = resolve_dispatch_prompt(&mut args).expect_err(prompt);
        if let Some(expected) = expected {
            assert!(error.message.contains(expected), "{error:?}");
        }
    }
}

#[test]
fn stdin_snapshot_literals_are_never_reparsed_as_structured_prompt_specs() {
    for literal in ["-", "@/missing/prompt.md", "@prompt:missing", " \n\t"] {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "-",
            "--backend",
            "fixture",
            "--no-finalize",
        ])
        .expect("parse Cook");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let crate::commands::agent_task::AgentTaskCommand::Cook(mut cook) = agent_task.command
        else {
            panic!("Cook command");
        };
        cook.prompt_snapshot = Some(super::super::args::CookPromptSnapshot {
            content: literal.to_string(),
            source: "stdin".to_string(),
            sha256: "sha256:test".to_string(),
            size_bytes: literal.len(),
        });

        let dispatch = resolved_dispatch_args_for_cook(&cook).expect("literal snapshot");
        assert_eq!(dispatch.prompt.as_deref(), Some(literal));
        assert_eq!(cook.dispatch.prompt.as_deref(), Some("-"));
    }
}
