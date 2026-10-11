use super::{CommandScopeConfig, Component};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeCommand {
    Lint,
    Test,
    Deploy,
    Release,
    Fleet,
}

#[derive(Debug, Clone, Default)]
pub struct EffectiveScope {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

pub fn resolve_component_scope(component: &Component, command: ScopeCommand) -> EffectiveScope {
    let mut effective = EffectiveScope::default();

    let Some(scopes) = component.scopes.as_ref() else {
        dedupe(&mut effective.include);
        dedupe(&mut effective.exclude);
        return effective;
    };

    if let Some(defaults) = scopes.defaults.as_ref() {
        merge_scope(&mut effective, defaults);
    }

    let command_scope = match command {
        ScopeCommand::Lint => scopes.lint.as_ref(),
        ScopeCommand::Test => scopes.test.as_ref(),
        ScopeCommand::Deploy => scopes.deploy.as_ref(),
        ScopeCommand::Release => scopes.release.as_ref(),
        ScopeCommand::Fleet => scopes.fleet.as_ref(),
    };

    if let Some(scope) = command_scope {
        merge_scope(&mut effective, scope);
    }

    dedupe(&mut effective.include);
    dedupe(&mut effective.exclude);
    effective
}

fn merge_scope(target: &mut EffectiveScope, scope: &CommandScopeConfig) {
    target.include.extend(scope.include.iter().cloned());
    target.exclude.extend(scope.exclude.iter().cloned());
}

fn dedupe(items: &mut Vec<String>) {
    items.sort();
    items.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, ScopeConfig};

    #[test]
    fn merges_default_and_command_specific_excludes() {
        let mut component = Component::new(
            "homeboy".to_string(),
            "/tmp/homeboy".to_string(),
            "".to_string(),
            None,
        );
        component.scopes = Some(ScopeConfig {
            defaults: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["tmp/**".to_string(), "docs/CHANGES.md".to_string()],
            }),
            deploy: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["docs/CHANGES.md".to_string()],
            }),
            ..Default::default()
        });

        let resolved = resolve_component_scope(&component, ScopeCommand::Deploy);
        assert_eq!(resolved.exclude, vec!["docs/CHANGES.md", "tmp/**"]);
    }

    #[test]
    fn scope_is_empty_without_component_scopes() {
        let component = Component::new(
            "generic".to_string(),
            "/tmp/generic".to_string(),
            "".to_string(),
            None,
        );

        let resolved = resolve_component_scope(&component, ScopeCommand::Deploy);
        assert!(resolved.exclude.is_empty());
        assert!(resolved.include.is_empty());
    }
}
