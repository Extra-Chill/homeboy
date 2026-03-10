use crate::component::{CommandScopeConfig, Component};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeCommand {
    Audit,
    Lint,
    Test,
    Refactor,
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
    let mut effective = builtin_scope_defaults(component, command);

    let Some(scopes) = component.scopes.as_ref() else {
        dedupe(&mut effective.include);
        dedupe(&mut effective.exclude);
        return effective;
    };

    if let Some(defaults) = scopes.defaults.as_ref() {
        merge_scope(&mut effective, defaults);
    }

    let command_scope = match command {
        ScopeCommand::Audit => scopes.audit.as_ref(),
        ScopeCommand::Lint => scopes.lint.as_ref(),
        ScopeCommand::Test => scopes.test.as_ref(),
        ScopeCommand::Refactor => scopes.refactor.as_ref(),
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

fn builtin_scope_defaults(component: &Component, command: ScopeCommand) -> EffectiveScope {
    let mut effective = EffectiveScope::default();

    if matches!(command, ScopeCommand::Audit) {
        effective.exclude.push("CHANGELOG.md".to_string());
        if let Some(target) = component.changelog_target.as_ref() {
            effective.exclude.push(target.clone());
        }
    }

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
                exclude: vec!["tmp/**".to_string()],
            }),
            audit: Some(CommandScopeConfig {
                include: vec![],
                exclude: vec!["docs/CHANGES.md".to_string()],
            }),
            ..Default::default()
        });
        component.changelog_target = Some("docs/CHANGELOG.md".to_string());

        let resolved = resolve_component_scope(&component, ScopeCommand::Audit);
        assert_eq!(
            resolved.exclude,
            vec!["docs/CHANGELOG.md", "docs/CHANGES.md", "tmp/**"]
        );
    }

    #[test]
    fn audit_scope_has_no_builtin_changelog_without_component_target() {
        let component = Component::new(
            "generic".to_string(),
            "/tmp/generic".to_string(),
            "".to_string(),
            None,
        );

        let resolved = resolve_component_scope(&component, ScopeCommand::Audit);
        assert!(resolved.exclude.is_empty());
    }

    #[test]
    fn audit_scope_includes_component_changelog_target() {
        let mut component = Component::new(
            "generic".to_string(),
            "/tmp/generic".to_string(),
            "".to_string(),
            None,
        );
        component.changelog_target = Some("docs/CHANGES.md".to_string());

        let resolved = resolve_component_scope(&component, ScopeCommand::Audit);
        assert_eq!(resolved.exclude, vec!["docs/CHANGES.md"]);
    }

    #[test]
    fn audit_scope_includes_builtin_changelog_default_for_all_components() {
        let component = Component::new(
            "generic".to_string(),
            "/tmp/generic".to_string(),
            "".to_string(),
            None,
        );

        let resolved = resolve_component_scope(&component, ScopeCommand::Audit);
        assert_eq!(resolved.exclude, vec!["CHANGELOG.md"]);
    }

    #[test]
    fn audit_scope_includes_component_changelog_target() {
        let mut component = Component::new(
            "generic".to_string(),
            "/tmp/generic".to_string(),
            "".to_string(),
            None,
        );
        component.changelog_target = Some("docs/CHANGES.md".to_string());

        let resolved = resolve_component_scope(&component, ScopeCommand::Audit);
        assert_eq!(resolved.exclude, vec!["CHANGELOG.md", "docs/CHANGES.md"]);
    }
}
