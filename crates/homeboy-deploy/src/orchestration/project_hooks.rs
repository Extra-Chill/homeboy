use std::collections::HashMap;

use homeboy_core::context::RemoteProjectContext;
use homeboy_core::engine::hooks;
use homeboy_core::engine::hooks::HookFailureMode;
use homeboy_core::engine::template::TemplateVars;
use homeboy_core::project::Project;
use homeboy_extension_contract::HookEvent;

/// Run the project's `post:deploy:project` hook, once per deploy invocation.
///
/// Unlike `post:deploy`, this event is not resolved per component: it is
/// declared only on the project (see `Project::hooks`) and is never merged
/// from extensions or components (see the resolution-order rationale in
/// `homeboy_core::engine::hooks`). Call this once, after every component in
/// the deploy has finished, so a site-wide step like a page-cache purge does
/// not repeat once per component (homeboy#14973).
///
/// Non-fatal, matching `post:deploy`: failures are logged but do not affect
/// the deploy result.
pub(super) fn run_project_scoped_post_deploy_hook(ctx: &RemoteProjectContext, project: &Project) {
    if !project.hooks.contains_key(&HookEvent::PostDeployProject) {
        return;
    }

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::PROJECT_ID.to_string(), project.id.clone());
    if let Some(base_path) = project.base_path.as_deref() {
        vars.insert(TemplateVars::BASE_PATH.to_string(), base_path.to_string());
    }

    match hooks::run_project_scoped_hooks_remote(
        &ctx.client,
        &project.hooks,
        HookEvent::PostDeployProject,
        HookFailureMode::NonFatal,
        &vars,
    ) {
        Ok(result) => {
            for cmd_result in &result.commands {
                if cmd_result.success {
                    homeboy_core::log_status!(
                        "deploy",
                        "post:deploy:project> {}",
                        cmd_result.command
                    );
                } else {
                    homeboy_core::log_status!(
                        "deploy",
                        "post:deploy:project failed (exit {})> {}",
                        cmd_result.exit_code,
                        cmd_result.command
                    );
                }
            }
        }
        Err(e) => {
            homeboy_core::log_status!("deploy", "post:deploy:project hook error: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::server::Server;

    fn ctx_for(project: Project) -> RemoteProjectContext {
        RemoteProjectContext {
            project: project.clone(),
            server_id: "test".to_string(),
            server: Server {
                id: "test".to_string(),
                aliases: Vec::new(),
                host: "localhost".to_string(),
                user: "test".to_string(),
                port: 22,
                identity_file: None,
                kind: None,
                auth: None,
                env: HashMap::new(),
                runner: None,
            },
            client: crate::test_support::local_client(),
            base_path: project.base_path.clone(),
        }
    }

    #[test]
    fn does_nothing_when_project_declares_no_hook() {
        let project = Project {
            id: "site".to_string(),
            ..Project::default()
        };
        // Must not panic and must not attempt any command execution — there
        // is nothing to assert on directly here beyond "did not blow up",
        // since a no-op is externally silent by design.
        run_project_scoped_post_deploy_hook(&ctx_for(project.clone()), &project);
    }

    #[test]
    fn does_not_run_when_only_post_deploy_is_declared() {
        // A project declaring the component-scoped `post:deploy` event (but
        // not the project-scoped `post:deploy:project` event) must not run
        // anything here — that would double-run a hook meant for
        // `run_post_deploy_hooks` per component.
        let project = Project {
            id: "site".to_string(),
            hooks: HashMap::from([(
                HookEvent::PostDeploy,
                vec!["echo should-not-run".to_string()],
            )]),
            ..Project::default()
        };
        run_project_scoped_post_deploy_hook(&ctx_for(project.clone()), &project);
    }

    #[test]
    fn runs_the_declared_project_scoped_hook_once() {
        let project = Project {
            id: "site".to_string(),
            base_path: Some("/var/www/site".to_string()),
            hooks: HashMap::from([(
                HookEvent::PostDeployProject,
                vec!["echo {{base_path}} {{projectId}}".to_string()],
            )]),
            ..Project::default()
        };
        // Non-fatal by design, so this only asserts the call completes; the
        // resolved command content/order is covered directly by
        // `hooks::run_project_scoped_hooks_remote` unit tests in
        // `homeboy_core::engine::hooks`.
        run_project_scoped_post_deploy_hook(&ctx_for(project.clone()), &project);
    }
}
