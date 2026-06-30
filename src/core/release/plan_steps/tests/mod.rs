use super::hints::{github_release_applies, push_publish_vs_github_release_hints};
use super::preflight::build_preflight_steps;
use super::release::build_release_steps;
use crate::core::component::{Component, ScopedExtensionConfig};
use crate::core::extension::ExtensionManifest;
use crate::core::plan::PlanStepStatus;
use crate::core::release::scope::ReleaseScope;
use crate::core::release::types::{
    ReleaseBumpPolicyOptions, ReleaseChangelogPlan, ReleaseOptions, ReleasePipelineOptions,
    ReleaseSemverRecommendation,
};

#[test]
fn test_build_preflight_steps() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };

    let steps = build_preflight_steps(&options, None, &[]);
    let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();

    assert_eq!(
        ids,
        vec![
            "preflight.default_branch",
            "preflight.git_identity",
            "preflight.working_tree",
            "preflight.remote_sync",
            "preflight.bump_policy",
            "preflight.audit",
            "preflight.lint",
            "preflight.test",
            "preflight.changelog_bootstrap"
        ]
    );
    assert_eq!(steps[0].status, PlanStepStatus::Ready);
    assert_eq!(steps[1].status, PlanStepStatus::Disabled);
    assert_eq!(steps[2].needs, vec!["preflight.git_identity"]);
}

#[test]
fn release_plan_marks_git_identity_ready_when_requested() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        git_identity: Some("Release Bot <bot@example.com>".to_string()),
        ..Default::default()
    };

    let steps = build_preflight_steps(&options, None, &[]);
    let identity = steps
        .iter()
        .find(|step| step.id == "preflight.git_identity")
        .expect("git identity step");

    assert_eq!(identity.status, PlanStepStatus::Ready);
    assert_eq!(identity.needs, vec!["preflight.default_branch"]);
    assert_eq!(
        identity
            .inputs
            .get("identity")
            .and_then(|value| value.as_str()),
        Some("Release Bot <bot@example.com>")
    );
}

#[test]
fn release_plan_adds_extension_declared_release_preflight() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };
    let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Registry",
        "version": "1.0.0",
        "release_preflights": [
            {
                "id": "publish_token",
                "label": "Validate registry publish token",
                "action": "release.preflight.publish-token",
                "needs": ["preflight.bump_policy"]
            }
        ],
        "actions": [
            {
                "id": "release.preflight.publish-token",
                "label": "Validate publish token",
                "type": "command",
                "command": "true"
            }
        ]
    }))
    .expect("extension manifest");
    extension.id = "registry".to_string();

    let steps = build_preflight_steps(&options, None, &[extension]);
    let token = steps
        .iter()
        .find(|step| step.id == "preflight.extension.registry.publish_token")
        .expect("extension release preflight");

    assert_eq!(token.status, PlanStepStatus::Ready);
    assert_eq!(
        token.label.as_deref(),
        Some("Validate registry publish token")
    );
    assert_eq!(token.needs, vec!["preflight.bump_policy"]);
    assert_eq!(
        token
            .inputs
            .get("extension")
            .and_then(|value| value.as_str()),
        Some("registry")
    );
    assert_eq!(
        token.inputs.get("action").and_then(|value| value.as_str()),
        Some("release.preflight.publish-token")
    );
}

#[test]
fn release_plan_marks_quality_preflights_disabled_when_checks_are_skipped() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        skip_checks: true,
        ..Default::default()
    };

    let steps = build_preflight_steps(&options, None, &[]);
    for step_id in ["preflight.audit", "preflight.lint", "preflight.test"] {
        let quality = steps
            .iter()
            .find(|step| step.id == step_id)
            .expect("quality step");

        assert_eq!(quality.status, PlanStepStatus::Disabled);
        assert_eq!(
            quality
                .inputs
                .get("reason")
                .and_then(|value| value.as_str()),
            Some("--skip-checks")
        );
    }
}

#[test]
fn head_release_with_artifacts_skips_branch_and_working_tree_checks() {
    let options = ReleaseOptions {
        bump_type: "head".to_string(),
        pipeline: ReleasePipelineOptions {
            head: true,
            from_artifacts: Some("artifacts".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    let steps = build_preflight_steps(&options, None, &[]);
    let default_branch = steps
        .iter()
        .find(|step| step.id == "preflight.default_branch")
        .expect("default branch step");

    assert_eq!(default_branch.status, PlanStepStatus::Disabled);
    assert_eq!(
        default_branch
            .inputs
            .get("reason")
            .and_then(|value| value.as_str()),
        Some("head-release")
    );

    let working_tree = steps
        .iter()
        .find(|step| step.id == "preflight.working_tree")
        .expect("working tree step");

    assert_eq!(working_tree.status, PlanStepStatus::Disabled);
    assert_eq!(
        working_tree
            .inputs
            .get("reason")
            .and_then(|value| value.as_str()),
        Some("head-release-artifacts")
    );
}

#[test]
fn release_plan_records_explicit_quality_preflights() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };

    let steps = build_preflight_steps(&options, None, &[]);
    let audit = steps
        .iter()
        .find(|step| step.id == "preflight.audit")
        .expect("audit step");
    let lint = steps
        .iter()
        .find(|step| step.id == "preflight.lint")
        .expect("lint step");
    let test = steps
        .iter()
        .find(|step| step.id == "preflight.test")
        .expect("test step");
    let changelog_bootstrap = steps
        .iter()
        .find(|step| step.id == "preflight.changelog_bootstrap")
        .expect("changelog bootstrap step");

    assert_eq!(audit.status, PlanStepStatus::Disabled);
    assert_eq!(
        audit.inputs.get("reason").and_then(|value| value.as_str()),
        Some("no-release-audit-policy")
    );
    assert_eq!(lint.status, PlanStepStatus::Ready);
    assert_eq!(lint.needs, vec!["preflight.bump_policy"]);
    assert_eq!(test.status, PlanStepStatus::Ready);
    assert_eq!(test.needs, vec!["preflight.lint"]);
    assert_eq!(changelog_bootstrap.needs, vec!["preflight.test"]);
}

#[test]
fn release_plan_records_unforced_lower_bump_policy() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };
    let recommendation = semver_recommendation("minor", "patch", true);

    let steps = build_preflight_steps(&options, Some(&recommendation), &[]);
    let bump_policy = steps
        .iter()
        .find(|step| step.id == "preflight.bump_policy")
        .expect("bump policy step");

    assert_eq!(bump_policy.status, PlanStepStatus::Ready);
    assert_eq!(bump_policy.needs, vec!["preflight.remote_sync"]);
    assert_eq!(
        bump_policy
            .inputs
            .get("recommended")
            .and_then(|value| value.as_str()),
        Some("minor")
    );
    assert_eq!(
        bump_policy
            .inputs
            .get("requested")
            .and_then(|value| value.as_str()),
        Some("patch")
    );
    assert_eq!(
        bump_policy
            .inputs
            .get("policy")
            .and_then(|value| value.as_str()),
        Some("requires-force-lower-bump")
    );
    assert_eq!(
        bump_policy
            .inputs
            .get("force_lower_bump")
            .and_then(|value| value.as_bool()),
        Some(false)
    );
}

#[test]
fn release_plan_records_forced_lower_bump_policy() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        bump_policy: ReleaseBumpPolicyOptions {
            force_lower_bump: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let recommendation = semver_recommendation("minor", "patch", true);

    let steps = build_preflight_steps(&options, Some(&recommendation), &[]);
    let bump_policy = steps
        .iter()
        .find(|step| step.id == "preflight.bump_policy")
        .expect("bump policy step");

    assert_eq!(
        bump_policy
            .inputs
            .get("policy")
            .and_then(|value| value.as_str()),
        Some("forced-lower-bump")
    );
    assert_eq!(
        bump_policy
            .inputs
            .get("force_lower_bump")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}

#[test]
fn release_plan_records_dry_run_lower_bump_preview() {
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        dry_run: true,
        ..Default::default()
    };
    let recommendation = semver_recommendation("minor", "patch", true);

    let steps = build_preflight_steps(&options, Some(&recommendation), &[]);
    let bump_policy = steps
        .iter()
        .find(|step| step.id == "preflight.bump_policy")
        .expect("bump policy step");

    assert_eq!(
        bump_policy
            .inputs
            .get("policy")
            .and_then(|value| value.as_str()),
        Some("preview-lower-bump")
    );
}

#[test]
fn test_build_release_steps() {
    let component = fixture_component();
    let extension = serde_json::from_value(serde_json::json!({
        "name": "Fixture",
        "version": "1.0.0",
        "actions": [
            {
                "id": "release.prepare",
                "label": "Prepare release",
                "type": "command",
                "command": "true"
            }
        ]
    }))
    .expect("extension manifest");
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };

    let steps = build_release_steps(
        &component,
        &[extension],
        "1.0.0",
        "1.0.1",
        &fixture_changelog_plan(),
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
    let changelog_policy_index = step_index(&ids, "changelog.policy");
    let changelog_index = step_index(&ids, "changelog.generate");
    let changelog_finalize_index = step_index(&ids, "changelog.finalize");
    let version_index = step_index(&ids, "version");
    let prepare_index = step_index(&ids, "release.prepare");
    let commit_index = step_index(&ids, "git.commit");

    assert!(changelog_policy_index < changelog_index);
    assert!(changelog_index < version_index);
    assert!(changelog_finalize_index < version_index);
    assert!(version_index < prepare_index);
    assert!(prepare_index < commit_index);
    assert_eq!(steps[changelog_index].needs, vec!["changelog.policy"]);
    assert_eq!(
        steps[changelog_finalize_index].needs,
        vec!["changelog.generate"]
    );
    assert_eq!(steps[version_index].needs, vec!["changelog.finalize"]);
    assert_eq!(steps[prepare_index].needs, vec!["version"]);
    assert_eq!(steps[commit_index].needs, vec!["release.prepare"]);
}

#[test]
fn release_plan_runs_package_preflight_before_mutating_release_steps() {
    let mut component = fixture_component();
    component.extensions = Some(std::collections::HashMap::from([(
        "fixture-packager".to_string(),
        ScopedExtensionConfig::default(),
    )]));
    let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Fixture Packager",
        "version": "1.0.0",
        "actions": [
            {
                "id": "release.package",
                "label": "Package release",
                "type": "command",
                "command": "true"
            },
            {
                "id": "release.publish",
                "label": "Publish release",
                "type": "command",
                "command": "true"
            }
        ]
    }))
    .expect("extension manifest");
    extension.id = "fixture-packager".to_string();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };

    let steps = build_release_steps(
        &component,
        &[extension],
        "1.0.0",
        "1.0.1",
        &fixture_changelog_plan(),
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
    let package_preflight_index = step_index(&ids, "preflight.package");
    let tag_preflight_index = step_index(&ids, "preflight.tag_availability");
    let changelog_finalize_index = step_index(&ids, "changelog.finalize");
    let version_index = step_index(&ids, "version");
    let commit_index = step_index(&ids, "git.commit");

    assert!(package_preflight_index < changelog_finalize_index);
    assert!(tag_preflight_index < changelog_finalize_index);
    assert!(package_preflight_index < version_index);
    assert!(tag_preflight_index < version_index);
    assert!(package_preflight_index < commit_index);
    assert!(tag_preflight_index < commit_index);

    let package_preflight = &steps[package_preflight_index];
    assert_eq!(
        package_preflight.needs,
        vec!["preflight.changelog_bootstrap"]
    );

    let changelog_policy = steps
        .iter()
        .find(|step| step.id == "changelog.policy")
        .expect("changelog policy step");
    assert_eq!(changelog_policy.needs, vec!["preflight.tag_availability"]);

    let tag_preflight = &steps[tag_preflight_index];
    assert_eq!(tag_preflight.needs, vec!["preflight.package"]);
    assert_eq!(
        tag_preflight
            .inputs
            .get("name")
            .and_then(|value| value.as_str()),
        Some("v1.0.1")
    );
}

#[test]
fn release_plan_records_changelog_contract() {
    let component = fixture_component();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };
    let changelog_plan = fixture_changelog_plan();

    let steps = build_release_steps(
        &component,
        &[],
        "1.0.0",
        "1.0.1",
        &changelog_plan,
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    let policy = steps
        .iter()
        .find(|step| step.id == "changelog.policy")
        .expect("changelog policy step");
    let generate = steps
        .iter()
        .find(|step| step.id == "changelog.generate")
        .expect("changelog generate step");
    let finalize = steps
        .iter()
        .find(|step| step.id == "changelog.finalize")
        .expect("changelog finalize step");

    assert_eq!(
        policy.inputs.get("policy").and_then(|value| value.as_str()),
        Some("generated")
    );
    assert_eq!(
        policy.inputs.get("path").and_then(|value| value.as_str()),
        Some("CHANGELOG.md")
    );
    assert_eq!(
        generate
            .inputs
            .get("entry_count")
            .and_then(|value| value.as_u64()),
        Some(1)
    );
    assert_eq!(
        finalize.inputs.get("mode").and_then(|value| value.as_str()),
        Some("version-step")
    );
    assert_eq!(
        finalize
            .inputs
            .get("entries")
            .and_then(|value| value.as_object())
            .and_then(|entries| entries.get("Fixed"))
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn release_plan_includes_deploy_intent_when_requested() {
    let component = fixture_component();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        pipeline: ReleasePipelineOptions {
            deploy: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let steps = build_release_steps(
        &component,
        &[],
        "1.0.0",
        "1.0.1",
        &fixture_changelog_plan(),
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    let deploy = steps
        .iter()
        .find(|step| step.id == "deploy")
        .expect("deploy step");
    assert_eq!(deploy.needs, vec!["git.push"]);
    assert_eq!(
        deploy
            .inputs
            .get("execution")
            .and_then(|value| value.as_str()),
        Some("release_plan")
    );
}

#[test]
fn head_release_plan_skips_mutation_steps_and_uses_existing_artifacts() {
    let mut component = fixture_component();
    component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
    let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "WordPress",
        "version": "1.0.0",
        "actions": [
            {
                "id": "release.publish",
                "label": "Publish release",
                "type": "command",
                "command": "true"
            }
        ]
    }))
    .expect("extension manifest");
    extension.id = "wordpress".to_string();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "head".to_string(),
        pipeline: ReleasePipelineOptions {
            head: true,
            from_artifacts: Some("artifacts".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    let steps = build_release_steps(
        &component,
        &[extension],
        "1.0.1",
        "1.0.1",
        &fixture_changelog_plan(),
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
    assert!(!ids.contains(&"changelog.finalize"));
    assert!(!ids.contains(&"version"));
    assert!(!ids.contains(&"git.commit"));
    assert!(!ids.contains(&"git.tag"));
    assert!(!ids.contains(&"git.push"));
    assert_eq!(
        ids,
        vec![
            "artifacts.inventory",
            "github.release",
            "publish.wordpress",
            "cleanup"
        ]
    );
    assert_eq!(
        steps[0].inputs.get("dir").and_then(|value| value.as_str()),
        Some("artifacts")
    );
    assert_eq!(steps[1].needs, vec!["artifacts.inventory"]);
    assert_eq!(steps[2].needs, vec!["artifacts.inventory"]);
}

#[test]
fn head_release_skip_publish_still_uploads_existing_artifacts() {
    let mut component = fixture_component();
    component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
    let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Node.js",
        "version": "1.0.0",
        "actions": [
            {
                "id": "release.publish",
                "label": "Publish release",
                "type": "command",
                "command": "true"
            }
        ]
    }))
    .expect("extension manifest");
    extension.id = "package-runtime".to_string();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "head".to_string(),
        pipeline: ReleasePipelineOptions {
            head: true,
            skip_publish: true,
            from_artifacts: Some("artifacts".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    let steps = build_release_steps(
        &component,
        &[extension],
        "1.0.1",
        "1.0.1",
        &fixture_changelog_plan(),
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    let ids: Vec<&str> = steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(ids, vec!["artifacts.inventory", "github.release"]);
    assert_eq!(steps[1].needs, vec!["artifacts.inventory"]);
}

#[test]
fn release_plan_warns_when_configured_extensions_have_no_publish_action() {
    let mut component = fixture_component();
    component.extensions = Some(std::collections::HashMap::from([(
        "wordpress".to_string(),
        ScopedExtensionConfig::default(),
    )]));
    let mut extension: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "WordPress",
        "version": "1.0.0",
        "actions": [
            {
                "id": "release.package",
                "label": "Package release",
                "type": "command",
                "command": "true"
            }
        ]
    }))
    .expect("extension manifest");
    extension.id = "wordpress".to_string();
    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let release_scope = ReleaseScope::resolve(&component, &component.id).expect("release scope");
    let options = ReleaseOptions {
        bump_type: "patch".to_string(),
        ..Default::default()
    };

    let _steps = build_release_steps(
        &component,
        &[extension],
        "1.0.0",
        "1.0.1",
        &fixture_changelog_plan(),
        &options,
        &release_scope,
        &mut warnings,
        &mut hints,
    )
    .expect("steps");

    assert!(warnings.iter().any(|warning| {
        warning.contains("configured extensions (wordpress)")
            && warning.contains("release.package")
            && warning.contains("no extension provides 'release.publish'")
    }));
}

#[test]
fn test_github_release_applies() {
    let mut github_component = fixture_component();
    github_component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
    let mut non_github_component = fixture_component();
    non_github_component.remote_url = Some("https://gitlab.example.com/acme/tool.git".to_string());

    assert!(github_release_applies(&github_component));
    assert!(!github_release_applies(&non_github_component));
}

fn github_fixture_component() -> Component {
    let mut component = fixture_component();
    component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
    component
}

fn options_with_flags(skip_publish: bool, skip_github_release: bool) -> ReleaseOptions {
    ReleaseOptions {
        pipeline: ReleasePipelineOptions {
            skip_publish,
            ..Default::default()
        },
        skip_github_release,
        ..Default::default()
    }
}

#[test]
fn skip_publish_alone_warns_github_release_still_created() {
    let component = github_fixture_component();
    let options = options_with_flags(true, false);
    let mut hints = Vec::new();

    push_publish_vs_github_release_hints(
        &component,
        &options,
        &["crates-io".to_string()],
        &mut hints,
    );

    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("--skip-publish")
                && hint.contains("registry/package publishing")),
        "should clarify --skip-publish is registry/package only: {hints:?}"
    );
    assert!(
        hints.iter().any(|hint| {
            hint.contains("does NOT skip the GitHub Release")
                && hint.contains("WILL still be created")
        }),
        "should explicitly state a GitHub Release will still be created: {hints:?}"
    );
}

#[test]
fn skip_publish_and_no_github_release_states_tag_only() {
    let component = github_fixture_component();
    let options = options_with_flags(true, true);
    let mut hints = Vec::new();

    push_publish_vs_github_release_hints(&component, &options, &[], &mut hints);

    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("tag-only") && hint.contains("no GitHub Release")),
        "both flags together must state tag-only / no release page: {hints:?}"
    );
    assert!(
        !hints
            .iter()
            .any(|hint| hint.contains("WILL still be created")),
        "must not claim a GitHub Release will be created when --no-github-release is set: {hints:?}"
    );
}

#[test]
fn no_github_release_alone_states_no_release_page() {
    let component = github_fixture_component();
    let options = options_with_flags(false, true);
    let mut hints = Vec::new();

    push_publish_vs_github_release_hints(&component, &options, &[], &mut hints);

    assert!(
        hints
            .iter()
            .any(|hint| hint.contains("--no-github-release") && hint.contains("no GitHub Release")),
        "should state no GitHub Release page will be created: {hints:?}"
    );
}

#[test]
fn default_flags_emit_no_terminology_hints() {
    let component = github_fixture_component();
    let options = options_with_flags(false, false);
    let mut hints = Vec::new();

    push_publish_vs_github_release_hints(&component, &options, &[], &mut hints);

    assert!(
        hints.is_empty(),
        "no clarification hints expected on a default release: {hints:?}"
    );
}

fn fixture_component() -> Component {
    Component {
        id: "fixture".to_string(),
        local_path: "/tmp/fixture".to_string(),
        ..Default::default()
    }
}

fn fixture_changelog_plan() -> ReleaseChangelogPlan {
    ReleaseChangelogPlan {
        policy: "generated".to_string(),
        path: "CHANGELOG.md".to_string(),
        dry_run: false,
        entries: std::collections::HashMap::from([(
            "Fixed".to_string(),
            vec!["Correct release output".to_string()],
        )]),
        entry_count: 1,
    }
}

fn semver_recommendation(
    recommended: &str,
    requested: &str,
    is_underbump: bool,
) -> ReleaseSemverRecommendation {
    ReleaseSemverRecommendation {
        latest_tag: Some("v1.0.0".to_string()),
        range: "v1.0.0..HEAD".to_string(),
        commits: vec![],
        recommended_bump: Some(recommended.to_string()),
        requested_bump: requested.to_string(),
        is_underbump,
        reasons: vec![],
    }
}

fn step_index(ids: &[&str], id: &str) -> usize {
    ids.iter()
        .position(|candidate| *candidate == id)
        .unwrap_or_else(|| panic!("missing {id} step"))
}
