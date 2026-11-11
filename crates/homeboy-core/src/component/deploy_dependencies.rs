//! Extension-driven deploy-order dependency discovery for components.
//!
//! Extensions declare `deploy.dependency_inference` rules (a component-relative
//! file plus a regex). Core evaluates them generically and never knows what
//! framework the rule describes.

use std::collections::BTreeSet;
use std::path::Path;

use homeboy_extension_contract::DeployDependencyInferenceRule;

use super::model::{render_remote_path_template, Component};
use crate::extension::catalog::load_extension_in_optional_root;

/// Dependency slugs discovered for `component` by its linked extensions.
///
/// Infallible by design: an unreadable file, missing extension, or invalid
/// pattern contributes nothing rather than failing the deploy.
pub fn infer_deploy_dependencies(component: &Component) -> Vec<String> {
    let local = Path::new(&component.local_path);
    if component.local_path.trim().is_empty() || !local.is_dir() {
        return Vec::new();
    }
    let Some(dir_name) = local.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let Some(extensions) = component.extensions.as_ref() else {
        return Vec::new();
    };

    let mut found = BTreeSet::new();
    for extension_id in extensions.keys() {
        let Ok(extension) = load_extension_in_optional_root(None, extension_id) else {
            continue;
        };
        for rule in extension.deploy_dependency_inference_rules() {
            found.extend(evaluate_rule(rule, local, &component.id, dir_name));
        }
    }
    found.into_iter().collect()
}

/// Evaluate one rule against a component directory.
pub fn evaluate_rule(
    rule: &DeployDependencyInferenceRule,
    local: &Path,
    component_id: &str,
    dir_name: &str,
) -> Vec<String> {
    let relative_file = render_remote_path_template(&rule.file, component_id, dir_name);
    let relative_path = Path::new(&relative_file);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Vec::new();
    }
    let Ok(content) = std::fs::read_to_string(local.join(relative_path)) else {
        return Vec::new();
    };
    let Ok(regex) = regex::RegexBuilder::new(&rule.pattern)
        .multi_line(true)
        .build()
    else {
        return Vec::new();
    };
    let separator = rule.separator.as_deref().unwrap_or(",");

    let mut slugs = Vec::new();
    for captures in regex.captures_iter(&content) {
        let Some(list) = captures.get(1) else {
            continue;
        };
        for slug in list.as_str().split(separator) {
            let slug = slug.trim();
            if !slug.is_empty() && !slugs.iter().any(|existing| existing == slug) {
                slugs.push(slug.to_string());
            }
        }
    }
    slugs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str) -> DeployDependencyInferenceRule {
        DeployDependencyInferenceRule {
            file: "{{dir_name}}.php".to_string(),
            pattern: pattern.to_string(),
            separator: None,
        }
    }

    #[test]
    fn captures_a_comma_separated_header_list() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("my-plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(
            plugin.join("my-plugin.php"),
            "<?php\n/**\n * Plugin Name: Mine\n * Requires Plugins: base-plugin, other-plugin \n */\n",
        )
        .unwrap();

        let slugs = evaluate_rule(
            &rule(r"^[ \t/*#@]*Requires Plugins:[ \t]*(.+?)[ \t]*$"),
            &plugin,
            "mine",
            "my-plugin",
        );
        assert_eq!(slugs, vec!["base-plugin", "other-plugin"]);
    }

    #[test]
    fn missing_file_traversal_and_bad_pattern_yield_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(evaluate_rule(&rule("(.+)"), dir.path(), "x", "x").is_empty());

        let mut escaping = rule("(.+)");
        escaping.file = "../etc/passwd".to_string();
        assert!(evaluate_rule(&escaping, dir.path(), "x", "x").is_empty());

        std::fs::write(dir.path().join("x.php"), "Requires Plugins: a").unwrap();
        assert!(evaluate_rule(&rule("("), dir.path(), "x", "x").is_empty());
    }

    /// The bundled WordPress extension orders plugins by their
    /// `Requires Plugins:` header with no per-project configuration.
    #[test]
    fn bundled_wordpress_rules_read_the_requires_plugins_header() {
        let manifest: homeboy_extension_contract::ExtensionManifest = serde_json::from_str(
            include_str!("../../../../extensions/wordpress/wordpress.json"),
        )
        .expect("bundled wordpress.json parses as an extension manifest");
        let rules = manifest.deploy_dependency_inference_rules();
        assert!(!rules.is_empty());

        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("extrachill-events");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(
            plugin.join("extrachill-events.php"),
            "<?php\n/**\n * Plugin Name: Extra Chill Events\n * Version: 0.127.1\n * Requires Plugins: data-machine, data-machine-events, extrachill-users\n */\n",
        )
        .unwrap();

        let mut slugs: Vec<String> = rules
            .iter()
            .flat_map(|rule| evaluate_rule(rule, &plugin, "extrachill-events", "extrachill-events"))
            .collect();
        slugs.sort();
        slugs.dedup();
        assert_eq!(
            slugs,
            vec!["data-machine", "data-machine-events", "extrachill-users"]
        );
    }
}
