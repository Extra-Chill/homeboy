use crate::project::Project;

pub fn apply_component_overrides(
    component: &crate::component::Component,
    project: &Project,
) -> crate::component::Component {
    let Some(overrides) = project.component_overrides.get(&component.id) else {
        return component.clone();
    };

    let mut merged = component.clone();

    if let Some(build_artifact) = &overrides.build_artifact {
        merged.build_artifact = Some(build_artifact.clone());
    }
    if let Some(extract_command) = &overrides.extract_command {
        merged.extract_command = Some(extract_command.clone());
    }
    if let Some(remote_owner) = &overrides.remote_owner {
        merged.remote_owner = Some(remote_owner.clone());
    }
    if let Some(deploy_strategy) = &overrides.deploy_strategy {
        merged.deploy_strategy = Some(deploy_strategy.clone());
    }
    if let Some(git_deploy) = &overrides.git_deploy {
        merged.git_deploy = Some(git_deploy.clone());
    }
    if !overrides.hooks.is_empty() {
        merged.hooks = overrides.hooks.clone();
    }
    if let Some(scopes) = &overrides.scopes {
        merged.scopes = Some(scopes.clone());
    }

    merged
}
