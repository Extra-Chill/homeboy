use homeboy::core::component::{Component, ComponentScriptsConfig, DependencyStackEdge};
use homeboy::core::deps::{self, DependencyUpdateOptions};
use homeboy::extensions::deps_provider::{self, ComposerAction};
use std::fs;
use tempfile::tempdir;

fn write_file(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn fixture_component(path: &std::path::Path) -> (&'static str, String) {
    ("fixture", path.display().to_string())
}

fn stack_component(id: &str, path: &str, edges: Vec<DependencyStackEdge>) -> Component {
    let mut component = Component::new(id.to_string(), path.to_string(), String::new(), None);
    component.dependency_stack = edges;
    component
}

fn script_stack_component(
    id: &str,
    path: &std::path::Path,
    status_json: &str,
    edges: Vec<DependencyStackEdge>,
) -> Component {
    let script = path.join("deps.sh");
    write_file(
        &script,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = status ]; then\ncat <<'JSON'\n{status_json}\nJSON\nfi\n"
        ),
    );
    let mut component = stack_component(id, &path.display().to_string(), edges);
    component.scripts = Some(ComponentScriptsConfig {
        deps: vec![format!("sh {}", script.display())],
        ..Default::default()
    });
    component
}

#[test]
fn status_reads_composer_direct_constraints_and_lock_details() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let (_, root_path) = fixture_component(root);

    write_file(
        &root.join("composer.json"),
        r#"{
            "name": "fixture/root",
            "require": {
                "php": ">=8.1",
                "fixture/prod": "^1.0"
            },
            "require-dev": {
                "fixture/dev": "dev-main"
            }
        }"#,
    );
    write_file(
        &root.join("composer.lock"),
        r#"{
            "packages": [
                {
                    "name": "fixture/prod",
                    "version": "1.2.3",
                    "source": { "reference": "prod-ref" }
                },
                {
                    "name": "fixture/transitive",
                    "version": "0.1.0",
                    "dist": { "reference": "transitive-ref" }
                }
            ],
            "packages-dev": [
                {
                    "name": "fixture/dev",
                    "version": "dev-main",
                    "source": { "reference": "dev-ref" }
                }
            ]
        }"#,
    );

    let status = deps::status(Some("fixture"), Some(&root_path), None).unwrap();

    assert_eq!(status.component_id, "fixture");
    assert_eq!(status.package_manager, "composer");
    assert_eq!(status.packages.len(), 3);

    let prod = status
        .packages
        .iter()
        .find(|package| package.name == "fixture/prod")
        .unwrap();
    assert_eq!(prod.manifest_section.as_deref(), Some("require"));
    assert_eq!(prod.constraint.as_deref(), Some("^1.0"));
    assert_eq!(prod.locked_version.as_deref(), Some("1.2.3"));
    assert_eq!(prod.locked_reference.as_deref(), Some("prod-ref"));

    let dev = status
        .packages
        .iter()
        .find(|package| package.name == "fixture/dev")
        .unwrap();
    assert_eq!(dev.manifest_section.as_deref(), Some("require-dev"));
    assert_eq!(dev.constraint.as_deref(), Some("dev-main"));
    assert_eq!(dev.locked_reference.as_deref(), Some("dev-ref"));

    let transitive = status
        .packages
        .iter()
        .find(|package| package.name == "fixture/transitive")
        .unwrap();
    assert_eq!(transitive.manifest_section, None);
    assert_eq!(transitive.constraint, None);
    assert_eq!(
        transitive.locked_reference.as_deref(),
        Some("transitive-ref")
    );
}

#[test]
fn status_filters_to_one_package() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let (_, root_path) = fixture_component(root);

    write_file(
        &root.join("composer.json"),
        r#"{
            "name": "fixture/root",
            "require": {
                "fixture/one": "^1.0",
                "fixture/two": "^2.0"
            }
        }"#,
    );
    write_file(
        &root.join("composer.lock"),
        r#"{ "packages": [], "packages-dev": [] }"#,
    );

    let status = deps::status(Some("fixture"), Some(&root_path), Some("fixture/two")).unwrap();

    assert_eq!(status.packages.len(), 1);
    assert_eq!(status.packages[0].name, "fixture/two");
    assert_eq!(status.packages[0].constraint.as_deref(), Some("^2.0"));
}

#[test]
fn test_composer_command_args() {
    assert_eq!(
        deps_provider::composer_command_args(
            "fixture/package",
            &ComposerAction::Require {
                constraint: "^2.0".to_string(),
            },
        ),
        vec![
            "require",
            "fixture/package:^2.0",
            "--with-dependencies",
            "--no-interaction",
        ]
    );

    assert_eq!(
        deps_provider::composer_command_args("fixture/package", &ComposerAction::Update),
        vec![
            "update",
            "fixture/package",
            "--with-dependencies",
            "--no-interaction",
        ]
    );
}

#[test]
fn stack_plan_walks_declared_downstream_edges_in_order() {
    let components = vec![
        stack_component(
            "block-format-bridge",
            "/repo/block-format-bridge",
            vec![DependencyStackEdge {
                upstream: "example-org/html-to-blocks-converter".to_string(),
                downstream: "block-format-bridge".to_string(),
                package: "example-org/html-to-blocks-converter".to_string(),
                update: None,
                rebuild: false,
                post_update: vec!["composer build".to_string()],
                test: vec!["homeboy test --path . --extension wordpress".to_string()],
            }],
        ),
        stack_component(
            "static-site-importer",
            "/repo/static-site-importer",
            vec![DependencyStackEdge {
                upstream: "block-format-bridge".to_string(),
                downstream: "static-site-importer".to_string(),
                package: "example-org/block-format-bridge".to_string(),
                update: Some("composer update example-org/block-format-bridge".to_string()),
                rebuild: false,
                post_update: Vec::new(),
                test: vec!["homeboy test --path . --extension wordpress".to_string()],
            }],
        ),
    ];

    let plan =
        deps::stack_plan_from_components("example-org/html-to-blocks-converter", &components)
            .unwrap();

    let steps = plan.planned_steps();

    assert_eq!(plan.step_count(), 2);
    assert_eq!(plan.step_count(), plan.plan.steps.len());
    assert_eq!(steps[0].downstream, "block-format-bridge");
    assert_eq!(steps[0].package, "example-org/html-to-blocks-converter");
    assert_eq!(
        steps[0].update_command,
        "homeboy deps update example-org/html-to-blocks-converter --path /repo/block-format-bridge"
    );
    assert_eq!(steps[0].post_update, vec!["composer build"]);
    assert_eq!(steps[1].downstream, "static-site-importer");
    assert_eq!(
        steps[1].update_command,
        "composer update example-org/block-format-bridge"
    );
}

#[test]
fn stack_plan_compatibility_fields_are_serialized_from_homeboy_plan() {
    let components = vec![
        stack_component(
            "upstream",
            "/repo/upstream",
            vec![DependencyStackEdge {
                upstream: "upstream".to_string(),
                downstream: "downstream".to_string(),
                package: "fixture/upstream".to_string(),
                update: None,
                rebuild: false,
                post_update: Vec::new(),
                test: Vec::new(),
            }],
        ),
        stack_component("downstream", "/repo/downstream", Vec::new()),
    ];
    let mut plan = deps::stack_plan_from_components("upstream", &components).unwrap();

    plan.plan.subject.component_id = Some("renamed-upstream".to_string());
    plan.plan.steps.clear();
    plan.plan.summary = None;

    let json = serde_json::to_value(&plan).unwrap();

    assert_eq!(json["upstream"], "renamed-upstream");
    assert_eq!(json["step_count"], 0);
    assert_eq!(json["steps"], serde_json::json!([]));
}

#[test]
fn stack_plan_derives_edges_from_provider_reported_dependency_identities() {
    let dir = tempdir().unwrap();
    let upstream_path = dir.path().join("upstream");
    let downstream_path = dir.path().join("downstream");
    fs::create_dir_all(&upstream_path).unwrap();
    fs::create_dir_all(&downstream_path).unwrap();

    let components = vec![
        script_stack_component(
            "upstream",
            &upstream_path,
            r#"{
                "package_manager": "fixture",
                "dependency_identities": ["fixture/upstream"],
                "packages": []
            }"#,
            Vec::new(),
        ),
        script_stack_component(
            "downstream",
            &downstream_path,
            r#"{
                "package_manager": "fixture",
                "packages": [
                    {
                        "name": "fixture/upstream",
                        "manifest_section": "dependencies",
                        "constraint": "^1.0"
                    }
                ]
            }"#,
            Vec::new(),
        ),
    ];

    let plan = deps::stack_plan_from_components("upstream", &components).unwrap();

    let steps = plan.planned_steps();

    assert_eq!(plan.step_count(), 1);
    assert_eq!(steps[0].declaring_component_id, "downstream");
    assert_eq!(steps[0].upstream, "upstream");
    assert_eq!(steps[0].downstream, "downstream");
    assert_eq!(steps[0].package, "fixture/upstream");
    assert_eq!(
        steps[0].update_command,
        format!(
            "homeboy deps update fixture/upstream --path {}",
            downstream_path.display()
        )
    );
}

#[test]
fn stack_plan_keeps_explicit_edge_config_when_provider_edge_matches() {
    let dir = tempdir().unwrap();
    let upstream_path = dir.path().join("upstream");
    let downstream_path = dir.path().join("downstream");
    fs::create_dir_all(&upstream_path).unwrap();
    fs::create_dir_all(&downstream_path).unwrap();

    let explicit_edge = DependencyStackEdge {
        upstream: "upstream".to_string(),
        downstream: "downstream".to_string(),
        package: "fixture/upstream".to_string(),
        update: Some("fixture-provider update fixture/upstream".to_string()),
        rebuild: false,
        post_update: vec!["fixture-provider build".to_string()],
        test: vec!["fixture-provider test".to_string()],
    };
    let components = vec![
        script_stack_component(
            "upstream",
            &upstream_path,
            r#"{
                "package_manager": "fixture",
                "dependency_identities": ["fixture/upstream"],
                "packages": []
            }"#,
            Vec::new(),
        ),
        script_stack_component(
            "downstream",
            &downstream_path,
            r#"{
                "package_manager": "fixture",
                "packages": [
                    {
                        "name": "fixture/upstream",
                        "manifest_section": "dependencies",
                        "constraint": "^1.0"
                    }
                ]
            }"#,
            vec![explicit_edge],
        ),
    ];

    let plan = deps::stack_plan_from_components("upstream", &components).unwrap();

    let steps = plan.planned_steps();

    assert_eq!(plan.step_count(), 1);
    assert_eq!(
        steps[0].update_command,
        "fixture-provider update fixture/upstream"
    );
    assert_eq!(steps[0].post_update, vec!["fixture-provider build"]);
    assert_eq!(steps[0].test, vec!["fixture-provider test"]);
}

#[test]
fn stack_plan_dedupes_cycles_by_edge_identity() {
    let components = vec![
        stack_component(
            "a",
            "/repo/a",
            vec![DependencyStackEdge {
                upstream: "a".to_string(),
                downstream: "b".to_string(),
                package: "fixture/b".to_string(),
                update: None,
                rebuild: false,
                post_update: Vec::new(),
                test: Vec::new(),
            }],
        ),
        stack_component(
            "b",
            "/repo/b",
            vec![DependencyStackEdge {
                upstream: "b".to_string(),
                downstream: "a".to_string(),
                package: "fixture/a".to_string(),
                update: None,
                rebuild: false,
                post_update: Vec::new(),
                test: Vec::new(),
            }],
        ),
    ];

    let plan = deps::stack_plan_from_components("a", &components).unwrap();

    let steps = plan.planned_steps();

    assert_eq!(plan.step_count(), 2);
    assert_eq!(steps[0].downstream, "b");
    assert_eq!(steps[1].downstream, "a");
}

#[test]
fn non_composer_component_returns_clear_unsupported_error() {
    let dir = tempdir().unwrap();
    let root_path = dir.path().display().to_string();

    let err = deps::status(Some("fixture"), Some(&root_path), None).unwrap_err();

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("dependency provider"));
    assert!(err.message.contains("No dependency provider found"));
}

#[test]
fn update_runs_component_provider_install_and_optional_rebuild() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("scripts")).unwrap();
    write_file(
        &root.join("homeboy.json"),
        r#"{
            "id": "fixture",
            "scripts": {
                "deps": ["sh scripts/deps.sh"],
                "build": ["sh scripts/build.sh"]
            }
        }"#,
    );
    write_file(
        &root.join("scripts/deps.sh"),
        r#"#!/bin/sh
case "$1" in
  update)
    printf '{"component_id":"ignored","component_path":"ignored","package_manager":"fixture","package":"ignored","requested_constraint":"ignored","command":["fixture-provider","update"],"stdout":"provider update","stderr":""}'
    ;;
  install)
    printf 'installed' > provider-install-marker
    printf 'provider install'
    ;;
  *)
    printf 'unknown deps action' >&2
    exit 2
    ;;
esac
"#,
    );
    write_file(
        &root.join("scripts/build.sh"),
        "#!/bin/sh\nprintf 'rebuilt' > build-marker\nprintf 'provider build'\n",
    );

    let root_path = root.display().to_string();
    let result = deps::update(
        Some("fixture"),
        Some(&root_path),
        "fixture/package",
        Some("^2.0"),
        DependencyUpdateOptions {
            install: true,
            rebuild: true,
        },
    )
    .unwrap();

    assert_eq!(result.package_manager, "fixture");
    assert_eq!(result.package, "fixture/package");
    assert_eq!(result.requested_constraint.as_deref(), Some("^2.0"));
    assert!(result.install.is_some());
    assert!(result.rebuild.is_some());
    assert_eq!(
        fs::read_to_string(root.join("provider-install-marker")).unwrap(),
        "installed"
    );
    assert_eq!(
        fs::read_to_string(root.join("build-marker")).unwrap(),
        "rebuilt"
    );
}

#[test]
fn deps_orchestration_stays_package_manager_agnostic() {
    let source = fs::read_to_string("src/core/deps.rs").unwrap();

    for forbidden in [
        "composer",
        "composer.json",
        "composer.lock",
        "Command::new",
        "Cargo",
    ] {
        assert!(
            !source.contains(forbidden),
            "core deps orchestration must not contain package-manager literal {forbidden:?}"
        );
    }
}

#[test]
#[ignore = "integration test mutates real composer manifests/locks and shells out to composer"]
fn update_with_constraint_changes_manifest_and_lock_for_local_path_package() {
    if std::process::Command::new("composer")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("composer not found; skipping integration-ish deps update test");
        return;
    }

    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let package = dir.path().join("package");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&package).unwrap();

    write_file(
        &package.join("composer.json"),
        r#"{
            "name": "fixture/package",
            "version": "1.0.0",
            "autoload": { "psr-4": { "Fixture\\Package\\": "src/" } }
        }"#,
    );
    fs::create_dir_all(package.join("src")).unwrap();
    write_file(
        &root.join("composer.json"),
        &format!(
            r#"{{
                "name": "fixture/root",
                "repositories": [
                    {{ "type": "path", "url": "{}", "options": {{ "symlink": false }} }}
                ],
                "require": {{ "fixture/package": "1.0.0" }}
            }}"#,
            package.display()
        ),
    );

    let initial = std::process::Command::new("composer")
        .args(["update", "--no-interaction"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        initial.status.success(),
        "initial composer update failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&initial.stdout),
        String::from_utf8_lossy(&initial.stderr)
    );

    write_file(
        &package.join("composer.json"),
        r#"{
            "name": "fixture/package",
            "version": "1.1.0",
            "autoload": { "psr-4": { "Fixture\\Package\\": "src/" } }
        }"#,
    );

    let root_path = root.display().to_string();
    let result = deps::update(
        Some("fixture"),
        Some(&root_path),
        "fixture/package",
        Some("1.1.0"),
        DependencyUpdateOptions {
            install: true,
            rebuild: false,
        },
    )
    .unwrap();

    assert_eq!(
        result.before.unwrap().locked_version.as_deref(),
        Some("1.0.0")
    );
    let after = result.after.unwrap();
    assert_eq!(after.constraint.as_deref(), Some("1.1.0"));
    assert_eq!(after.locked_version.as_deref(), Some("1.1.0"));
    assert_eq!(
        result.command,
        vec![
            "composer",
            "require",
            "fixture/package:1.1.0",
            "--with-dependencies",
            "--no-interaction",
        ]
    );
}
