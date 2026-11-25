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
    fn runs_only_the_project_scoped_hook_with_project_vars() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_string_lossy().to_string();
        let project = Project {
            id: "site".to_string(),
            base_path: Some(base.clone()),
            hooks: HashMap::from([
                (
                    HookEvent::PostDeploy,
                    vec!["touch {{base_path}}/component-hook".to_string()],
                ),
                (
                    HookEvent::PostDeployProject,
                    vec!["echo {{projectId}} > {{base_path}}/project-hook".to_string()],
                ),
            ]),
            ..Project::default()
        };

        run_project_scoped_post_deploy_hook(&ctx_for(project.clone()), &project);

        let marker = std::fs::read_to_string(dir.path().join("project-hook"))
            .expect("post:deploy:project hook must run");
        assert_eq!(marker.trim(), "site");
        assert!(
            !dir.path().join("component-hook").exists(),
            "component-scoped post:deploy must not run from the project-scoped hook"
        );
    }
}
