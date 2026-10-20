use homeboy::core::component::{Component, ComponentScriptsConfig, DependencyStackEdge};
use homeboy::core::deps::{self, DependencyInstallInvocation, DependencyUpdateOptions};
use std::fs;
use tempfile::tempdir;

// The component-script/build runners live in the extension subsystem, which is
// a separate crate registered at binary startup via `cli_runtime`. Integration
// tests that exercise extension-backed dependency behavior must register those
// providers explicitly (registration is an idempotent Mutex-backed slot).
//
// Every test that reaches an extension-backed runner path (a `script_stack_component`
// whose scripts report dependencies, or a `deps::update` that runs a provider
// install script) registers the runners itself rather than relying on a sibling
// test having populated the process-global slot first. Depending on that leak
// made results order- and scope-dependent: under a changed-scope/differential
// test run that excludes a registering sibling, these tests failed with "no
// component-script runner registered" (#8964).
fn register_component_script_runner() {
    homeboy_core::extension::component_script::register_component_script_runner();
    homeboy_core::extension::build::register_component_build_runner();
}

fn write_file(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn make_executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut mode = fs::metadata(path).unwrap().permissions();
        mode.set_mode(0o755);
        fs::set_permissions(path, mode).unwrap();
    }
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
fn status_prefers_neutral_adapter_manifest_over_builtin_manifests() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let root_path = root.display().to_string();

    write_file(
        &root.join("homeboy.json"),
        r#"{"id":"fixture","deps":{
            "provider": "fixture-adapter",
            "dependency_identities": ["fixture/root"],
            "packages": [
                {
                    "name": "fixture/package",
                    "manifest_section": "adapter-dependencies",
                    "constraint": "^1.0",
                    "locked_version": "1.2.3",
                    "locked_reference": "adapter-ref"
                }
            ],
            "commands": {
                "install": { "argv": ["fixture-adapter", "install"] }
            }
        }}"#,
    );
    write_file(
        &root.join("composer.json"),
        r#"{ "require": { "fixture/composer": "^9.0" } }"#,
    );
    write_file(
        &root.join("package.json"),
        r#"{ "dependencies": { "fixture-npm": "^9.0.0" } }"#,
    );

    let status = deps::status(Some("fixture"), Some(&root_path), None).unwrap();

    assert_eq!(status.package_manager, "fixture-adapter");
    assert_eq!(status.packages.len(), 1);
    assert_eq!(status.packages[0].name, "fixture/package");
    assert_eq!(status.packages[0].constraint.as_deref(), Some("^1.0"));
}

#[test]
fn neutral_adapter_manifest_discovers_install_command_and_runs_update_install() {
    register_component_script_runner();
    let dir = tempdir().unwrap();
    let root = dir.path();
    let root_path = root.display().to_string();
    let adapter = root.join("fixture-adapter.sh");
    write_file(
        &adapter,
        r#"#!/bin/sh
printf '%s\n' "$@" >> adapter-args.txt
"#,
    );
    make_executable(&adapter);
    write_file(
        &root.join("homeboy.json"),
        &format!(
            r#"{{"id":"fixture","deps":{{
                "provider": "fixture-adapter",
                "packages": [{{
                    "name": "fixture/package",
                    "manifest_section": "adapter-dependencies",
                    "constraint": "^1.0",
                    "locked_version": "1.2.3"
                }}],
                "commands": {{
                    "install": {{ "argv": ["{}", "install"] }},
                    "update": {{ "argv": ["{}", "update", "{{package}}", "{{constraint}}"] }}
                }}
            }}}}"#,
            adapter.display(),
            adapter.display()
        ),
    );

    let plan = deps::dependency_install_plan(root).unwrap();
    let install_command = vec![adapter.display().to_string(), "install".to_string()];
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].provider_id, "fixture-adapter");
    assert_eq!(
        plan[0].invocation,
        DependencyInstallInvocation::Argv {
            argv: install_command.clone()
        }
    );

    let install = deps::install(Some("fixture"), Some(&root_path)).unwrap();
    assert_eq!(install.package_manager, "fixture-adapter");
    assert_eq!(install.installs.len(), 1);
    assert_eq!(install.installs[0].command, install_command);

    let update = deps::update(
        Some("fixture"),
        Some(&root_path),
        "fixture/package",
        Some("^2.0"),
        DependencyUpdateOptions {
            install: false,
            rebuild: false,
        },
    )
    .unwrap();
    assert_eq!(update.package_manager, "fixture-adapter");
    assert_eq!(update.package, "fixture/package");
    assert_eq!(update.requested_constraint.as_deref(), Some("^2.0"));
    assert_eq!(
        update.command,
        vec![
            adapter.display().to_string(),
            "update".to_string(),
            "fixture/package".to_string(),
            "^2.0".to_string(),
        ]
    );
    assert_eq!(
        fs::read_to_string(root.join("adapter-args.txt")).unwrap(),
        "install\nupdate\nfixture/package\n^2.0\n"
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
                test: vec!["homeboy review test --path . --extension sample-runtime".to_string()],
                source: None,
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
                test: vec!["homeboy review test --path . --extension sample-runtime".to_string()],
                source: None,
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
                source: None,
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
    register_component_script_runner();
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
    register_component_script_runner();
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
        source: None,
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
                source: None,
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
                source: None,
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
fn stack_apply_dry_run_passes_constraint_to_default_provider_update() {
    let plan = deps::DependencyStackPlan::new(
        "upstream",
        vec![deps::DependencyStackPlanStep {
            sequence: 1,
            declaring_component_id: "downstream".to_string(),
            upstream: "upstream".to_string(),
            downstream: "downstream".to_string(),
            downstream_path: "/tmp/downstream path".to_string(),
            package: "fixture/upstream".to_string(),
            update_command: "homeboy deps update fixture/upstream --path '/tmp/downstream path'"
                .to_string(),
            rebuild: false,
            post_update: Vec::new(),
            test: Vec::new(),
        }],
    );

    let result = deps::stack_apply_plan(plan, Some("^2.0"), true, false, false).unwrap();

    assert_eq!(result.step_count, 1);
    assert_eq!(
        result.steps[0].command_results[0].command,
        "homeboy deps update fixture/upstream --path '/tmp/downstream path' --to '^2.0' --no-install"
    );
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
    register_component_script_runner();
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

// --- deps stack outdated ----------------------------------------------------

/// Run git against a real fixture repository with a hermetic identity: an
/// isolated HOME and an empty GIT_CONFIG_GLOBAL.
fn git_fixture(dir: &std::path::Path, args: &[&str]) {
    let home = dir.join("git-home");
    let global_config = dir.join("git-config-global");
    fs::create_dir_all(&home).unwrap();
    if !global_config.exists() {
        fs::write(&global_config, "").unwrap();
    }
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", &global_config)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A real git repository playing the upstream. Tags are what the remote
/// release resolution answers against, so the fixture is an actual repository
/// with actual tags, not a mock.
fn upstream_repo_with_tags(path: &std::path::Path, tags: &[&str]) {
    fs::create_dir_all(path).unwrap();
    git_fixture(path, &["init", "-q", "-b", "main"]);
    write_file(&path.join("README.md"), "fixture upstream\n");
    git_fixture(path, &["add", "."]);
    git_fixture(
        path,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    );
    for tag in tags {
        git_fixture(path, &["tag", tag]);
    }
}

fn outdated_edge(
    package: &str,
    source: Option<homeboy::core::component::DependencyStackEdgeSource>,
) -> DependencyStackEdge {
    DependencyStackEdge {
        upstream: "upstream".to_string(),
        downstream: "downstream".to_string(),
        package: package.to_string(),
        update: None,
        rebuild: false,
        post_update: Vec::new(),
        test: Vec::new(),
        source,
    }
}

/// A downstream whose locked versions come through the existing dependency
/// provider path: a neutral `homeboy.json` `deps` manifest provider, the same
/// product-agnostic provider surface the stack verbs already read.
fn outdated_downstream(
    path: &std::path::Path,
    locked: &[(&str, &str)],
    edges: Vec<DependencyStackEdge>,
) -> Component {
    fs::create_dir_all(path).unwrap();
    let packages = locked
        .iter()
        .map(|(name, version)| {
            format!(
                r#"{{"name":"{name}","manifest_section":"adapter-dependencies","constraint":"{version}","locked_version":"{version}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    write_file(
        &path.join("homeboy.json"),
        &format!(
            r#"{{"id":"downstream","deps":{{"provider":"fixture-outdated-adapter","packages":[{packages}]}}}}"#
        ),
    );
    stack_component("downstream", &path.display().to_string(), edges)
}

fn edge_by_package<'a>(
    status: &'a deps::DependencyStackOutdatedStatus,
    package: &str,
) -> &'a deps::DependencyStackEdgeOutdatedStatus {
    status
        .edges
        .iter()
        .find(|edge| edge.package == package)
        .unwrap_or_else(|| panic!("no edge reported for package {package}"))
}

#[test]
fn stack_outdated_reports_behind_current_v_prefixed_and_unresolvable_against_real_tags() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.17.0", "transformer-v0.18.0"]);

    let downstream_path = dir.path().join("downstream");
    let repo = upstream.display().to_string();
    let source = |prefix: &str| {
        Some(homeboy::core::component::DependencyStackEdgeSource {
            repo: repo.clone(),
            prefix: prefix.to_string(),
        })
    };
    let component = outdated_downstream(
        &downstream_path,
        &[
            ("fixture/behind", "0.17.0"),
            ("fixture/current", "0.18.0"),
            ("fixture/v-prefixed", "v0.18.0"),
            ("fixture/unresolvable", "0.18.0"),
        ],
        vec![
            outdated_edge("fixture/behind", source("transformer")),
            outdated_edge("fixture/current", source("transformer")),
            outdated_edge("fixture/v-prefixed", source("transformer")),
            outdated_edge("fixture/unresolvable", source("no-such-prefix")),
        ],
    );

    let status = deps::stack_outdated_for_component(&component, &downstream_path).unwrap();

    assert_eq!(status.edge_count, 4);

    let behind = edge_by_package(&status, "fixture/behind");
    assert_eq!(behind.state, deps::DependencyStackEdgeOutdatedState::Behind);
    assert_eq!(behind.locked_version.as_deref(), Some("0.17.0"));
    assert_eq!(behind.newest_version.as_deref(), Some("0.18.0"));
    assert_eq!(behind.newest_tag.as_deref(), Some("transformer-v0.18.0"));

    assert_eq!(
        edge_by_package(&status, "fixture/current").state,
        deps::DependencyStackEdgeOutdatedState::Current
    );

    // A lock recording `v0.18.0` for a `0.18.0` release is the same version.
    let v_prefixed = edge_by_package(&status, "fixture/v-prefixed");
    assert_eq!(v_prefixed.locked_version.as_deref(), Some("v0.18.0"));
    assert_eq!(
        v_prefixed.state,
        deps::DependencyStackEdgeOutdatedState::Current
    );

    let unresolvable = edge_by_package(&status, "fixture/unresolvable");
    assert_eq!(
        unresolvable.state,
        deps::DependencyStackEdgeOutdatedState::Unresolvable
    );
    assert!(
        unresolvable
            .unresolvable_reason
            .as_deref()
            .unwrap()
            .contains("no release tags matching prefix 'no-such-prefix'"),
        "unexpected reason: {:?}",
        unresolvable.unresolvable_reason
    );

    assert_eq!(status.behind_count, 1);
    assert_eq!(status.current_count, 2);
    assert_eq!(status.unresolvable_count, 1);
    assert_eq!(status.exit_code(), 1, "a behind edge gates with exit 1");
}

#[test]
fn stack_outdated_exit_code_distinguishes_all_current_from_unresolvable() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.18.0"]);
    let repo = upstream.display().to_string();
    let downstream_path = dir.path().join("downstream");

    // Every edge current -> exit 0.
    let all_current = outdated_downstream(
        &dir.path().join("downstream-all-current"),
        &[("fixture/current", "0.18.0")],
        vec![outdated_edge(
            "fixture/current",
            Some(homeboy::core::component::DependencyStackEdgeSource {
                repo: repo.clone(),
                prefix: "transformer".to_string(),
            }),
        )],
    );
    let status = deps::stack_outdated_for_component(
        &all_current,
        &dir.path().join("downstream-all-current"),
    )
    .unwrap();
    assert_eq!(status.behind_count, 0);
    assert_eq!(status.unresolvable_count, 0);
    assert_eq!(status.exit_code(), 0);

    // An edge without a remote source cannot be checked: unresolvable, never
    // current, and the run exits 2 because nothing is behind but the gate
    // cannot claim all-current.
    let sourceless = outdated_downstream(
        &downstream_path,
        &[("fixture/current", "0.18.0")],
        vec![
            outdated_edge(
                "fixture/current",
                Some(homeboy::core::component::DependencyStackEdgeSource {
                    repo: repo.clone(),
                    prefix: "transformer".to_string(),
                }),
            ),
            outdated_edge("fixture/untracked", None),
        ],
    );
    let status = deps::stack_outdated_for_component(&sourceless, &downstream_path).unwrap();

    let unresolvable = edge_by_package(&status, "fixture/untracked");
    assert_eq!(
        unresolvable.state,
        deps::DependencyStackEdgeOutdatedState::Unresolvable
    );
    assert!(
        unresolvable
            .unresolvable_reason
            .as_deref()
            .unwrap()
            .contains("declares no remote source"),
        "unexpected reason: {:?}",
        unresolvable.unresolvable_reason
    );
    assert_eq!(status.behind_count, 0);
    assert_eq!(status.unresolvable_count, 1);
    assert_eq!(status.exit_code(), 2);
}

#[test]
fn stack_outdated_marks_untracked_packages_unresolvable_not_current() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.18.0"]);
    let downstream_path = dir.path().join("downstream");

    // The provider reports a lock for fixture/current but nothing for
    // fixture/unknown, and fixture/nonsemver records a non-semver lock.
    let component = outdated_downstream(
        &downstream_path,
        &[
            ("fixture/current", "0.18.0"),
            ("fixture/nonsemver", "dev-main"),
        ],
        vec![
            outdated_edge(
                "fixture/unknown",
                Some(homeboy::core::component::DependencyStackEdgeSource {
                    repo: upstream.display().to_string(),
                    prefix: "transformer".to_string(),
                }),
            ),
            outdated_edge(
                "fixture/nonsemver",
                Some(homeboy::core::component::DependencyStackEdgeSource {
                    repo: upstream.display().to_string(),
                    prefix: "transformer".to_string(),
                }),
            ),
        ],
    );

    let status = deps::stack_outdated_for_component(&component, &downstream_path).unwrap();

    let unknown = edge_by_package(&status, "fixture/unknown");
    assert_eq!(
        unknown.state,
        deps::DependencyStackEdgeOutdatedState::Unresolvable
    );
    assert!(
        unknown
            .unresolvable_reason
            .as_deref()
            .unwrap()
            .contains("no dependency provider reports locked version"),
        "unexpected reason: {:?}",
        unknown.unresolvable_reason
    );

    let nonsemver = edge_by_package(&status, "fixture/nonsemver");
    assert_eq!(
        nonsemver.state,
        deps::DependencyStackEdgeOutdatedState::Unresolvable
    );
    assert!(
        nonsemver
            .unresolvable_reason
            .as_deref()
            .unwrap()
            .contains("not a comparable semver"),
        "unexpected reason: {:?}",
        nonsemver.unresolvable_reason
    );
    assert_eq!(status.exit_code(), 2);
}

#[test]
fn stack_outdated_dedupes_and_skips_edges_declared_for_other_downstreams() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.18.0"]);
    let downstream_path = dir.path().join("downstream");
    let repo = upstream.display().to_string();

    let mut component = outdated_downstream(
        &downstream_path,
        &[("fixture/current", "0.18.0")],
        vec![
            outdated_edge(
                "fixture/current",
                Some(homeboy::core::component::DependencyStackEdgeSource {
                    repo: repo.clone(),
                    prefix: "transformer".to_string(),
                }),
            ),
            outdated_edge(
                "fixture/current",
                Some(homeboy::core::component::DependencyStackEdgeSource {
                    repo: repo.clone(),
                    prefix: "transformer".to_string(),
                }),
            ),
        ],
    );
    // Push-model declaration for a different downstream: not this component's
    // lock to check, so the consumer-facing verb skips it.
    let mut foreign = outdated_edge("fixture/current", None);
    foreign.downstream = "other-downstream".to_string();
    component.dependency_stack.push(foreign);

    let status = deps::stack_outdated_for_component(&component, &downstream_path).unwrap();

    assert_eq!(status.edge_count, 1);
    assert_eq!(status.exit_code(), 0);
}

// --- deps stack refresh -----------------------------------------------------

/// Add a commit (and optional tags on it) to an existing upstream fixture
/// repository, so a release range carries real commit messages.
fn upstream_commit_with_tags(path: &std::path::Path, message: &str, tags: &[&str]) {
    write_file(
        &path.join("CHANGELOG.md"),
        &format!("fixture change: {message}\n"),
    );
    git_fixture(path, &["add", "."]);
    git_fixture(
        path,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
    for tag in tags {
        git_fixture(path, &["tag", tag]);
    }
}

fn refresh_edge_by_package<'a>(
    result: &'a deps::DependencyStackRefreshResult,
    package: &str,
) -> &'a deps::DependencyStackEdgeRefreshResult {
    result
        .edges
        .iter()
        .find(|edge| edge.package == package)
        .unwrap_or_else(|| panic!("no refresh edge reported for package {package}"))
}

#[test]
fn stack_refresh_updates_behind_edge_and_leaves_current_untouched() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.17.0", "transformer-v0.18.0"]);
    let repo = upstream.display().to_string();
    let downstream_path = dir.path().join("downstream");
    let source = |prefix: &str| {
        Some(homeboy::core::component::DependencyStackEdgeSource {
            repo: repo.clone(),
            prefix: prefix.to_string(),
        })
    };

    let mut behind = outdated_edge("fixture/behind", source("transformer"));
    behind.update = Some("touch refresh-update-marker".to_string());
    behind.post_update = vec!["touch refresh-post-marker".to_string()];
    let mut current = outdated_edge("fixture/current", source("transformer"));
    current.update = Some("touch refresh-current-marker".to_string());

    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "0.17.0"), ("fixture/current", "0.18.0")],
        vec![behind, current],
    );

    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, false, true).unwrap();

    assert_eq!(result.edge_count, 2);
    assert_eq!(result.updated_count, 1);
    assert_eq!(result.current_count, 1);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.unresolvable_count, 0);
    assert_eq!(result.exit_code(), 0, "a clean refresh gates with exit 0");

    let updated = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(
        updated.state,
        deps::DependencyStackEdgeRefreshState::Updated
    );
    assert_eq!(updated.from.as_deref(), Some("0.17.0"));
    assert_eq!(updated.to.as_deref(), Some("0.18.0"));
    // 0.x minors carry behavior changes: breaking without range inspection.
    assert_eq!(updated.breaking, Some(true));
    assert!(
        updated.breaking_reasons[0].contains("0.x minor changes"),
        "unexpected reasons: {:?}",
        updated.breaking_reasons
    );
    assert!(result.breaking, "an updated breaking edge marks the run");
    let phases: Vec<&str> = updated
        .command_results
        .iter()
        .map(|command| command.phase.as_str())
        .collect();
    assert_eq!(phases, vec!["update", "post_update"]);
    assert!(updated
        .command_results
        .iter()
        .all(|command| !command.skipped));
    assert!(downstream_path.join("refresh-update-marker").exists());
    assert!(downstream_path.join("refresh-post-marker").exists());

    let untouched = refresh_edge_by_package(&result, "fixture/current");
    assert_eq!(
        untouched.state,
        deps::DependencyStackEdgeRefreshState::Current
    );
    assert_eq!(untouched.breaking, Some(false));
    assert!(untouched.command_results.is_empty());
    assert!(
        !downstream_path.join("refresh-current-marker").exists(),
        "a current edge's update path must not run"
    );
}

#[test]
fn stack_refresh_dry_run_mutates_nothing() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.17.0", "transformer-v0.18.0"]);
    let downstream_path = dir.path().join("downstream");

    let mut behind = outdated_edge(
        "fixture/behind",
        Some(homeboy::core::component::DependencyStackEdgeSource {
            repo: upstream.display().to_string(),
            prefix: "transformer".to_string(),
        }),
    );
    behind.update = Some("touch refresh-update-marker".to_string());
    behind.post_update = vec!["touch refresh-post-marker".to_string()];

    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "0.17.0")],
        vec![behind],
    );

    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, true, true).unwrap();

    assert!(result.dry_run);
    assert_eq!(result.exit_code(), 0);
    let planned = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(
        planned.state,
        deps::DependencyStackEdgeRefreshState::Updated
    );
    assert!(
        planned
            .command_results
            .iter()
            .all(|command| command.skipped),
        "dry run must skip every command"
    );
    // The breaking verdict is still computed: it is read-only.
    assert_eq!(planned.breaking, Some(true));
    assert!(
        !downstream_path.join("refresh-update-marker").exists()
            && !downstream_path.join("refresh-post-marker").exists(),
        "dry run must not run edge commands"
    );
}

#[test]
fn stack_refresh_reports_failed_edges_and_gates_on_exit_status() {
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.17.0", "transformer-v0.18.0"]);
    let repo = upstream.display().to_string();
    let downstream_path = dir.path().join("downstream");

    let mut failing = outdated_edge(
        "fixture/behind",
        Some(homeboy::core::component::DependencyStackEdgeSource {
            repo: repo.clone(),
            prefix: "transformer".to_string(),
        }),
    );
    failing.update = Some("exit 7".to_string());
    failing.post_update = vec!["touch refresh-post-marker".to_string()];

    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "0.17.0")],
        vec![failing, outdated_edge("fixture/untracked", None)],
    );

    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, false, true).unwrap();

    let failed = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(failed.state, deps::DependencyStackEdgeRefreshState::Failed);
    assert!(
        failed.failure.as_deref().unwrap().contains("update"),
        "unexpected failure: {:?}",
        failed.failure
    );
    assert!(
        !downstream_path.join("refresh-post-marker").exists(),
        "post_update must not run after a failed update"
    );

    // An edge that cannot be checked never reads as current.
    let unresolvable = refresh_edge_by_package(&result, "fixture/untracked");
    assert_eq!(
        unresolvable.state,
        deps::DependencyStackEdgeRefreshState::Unresolvable
    );
    assert!(unresolvable.unresolvable_reason.is_some());
    assert_eq!(unresolvable.breaking, None);

    assert_eq!(result.failed_count, 1);
    assert_eq!(result.unresolvable_count, 1);
    assert_eq!(result.exit_code(), 1, "a failed edge gates with exit 1");
    assert!(result
        .gate_failure_message()
        .contains("fixture/behind failed"));

    // Nothing failed, one unresolvable -> exit 2.
    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "0.18.0")],
        vec![outdated_edge("fixture/untracked", None)],
    );
    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, false, true).unwrap();
    assert_eq!(result.unresolvable_count, 1);
    assert_eq!(
        result.exit_code(),
        2,
        "an unresolvable edge with no failures gates with exit 2"
    );
}

#[test]
fn stack_refresh_default_update_path_targets_the_newest_release() {
    register_component_script_runner();
    let dir = tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    upstream_repo_with_tags(&upstream, &["transformer-v0.17.0", "transformer-v0.18.0"]);
    let downstream_path = dir.path().join("downstream");
    fs::create_dir_all(&downstream_path).unwrap();

    let adapter = downstream_path.join("fixture-adapter.sh");
    write_file(
        &adapter,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> adapter-args.txt\n",
    );
    make_executable(&adapter);
    write_file(
        &downstream_path.join("homeboy.json"),
        &format!(
            r#"{{"id":"downstream","deps":{{
                "provider": "fixture-adapter",
                "packages": [{{
                    "name": "fixture/behind",
                    "manifest_section": "adapter-dependencies",
                    "constraint": "0.17.0",
                    "locked_version": "0.17.0"
                }}],
                "commands": {{
                    "update": {{ "argv": ["{adapter}", "update", "{{package}}", "{{constraint}}"] }}
                }}
            }}}}"#,
            adapter = adapter.display()
        ),
    );

    // No custom `update` on the edge: the provider-backed default update runs
    // with `--to <newest>`.
    let component = stack_component(
        "downstream",
        &downstream_path.display().to_string(),
        vec![outdated_edge(
            "fixture/behind",
            Some(homeboy::core::component::DependencyStackEdgeSource {
                repo: upstream.display().to_string(),
                prefix: "transformer".to_string(),
            }),
        )],
    );

    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, false, false).unwrap();

    let updated = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(
        updated.state,
        deps::DependencyStackEdgeRefreshState::Updated
    );
    assert!(
        updated.command_results[0].command.contains("--to 0.18.0"),
        "default update must target the newest release: {}",
        updated.command_results[0].command
    );
    assert!(
        updated.command_results[0].command.contains("--no-install"),
        "install=false must reach the default update: {}",
        updated.command_results[0].command
    );
    assert_eq!(
        fs::read_to_string(downstream_path.join("adapter-args.txt")).unwrap(),
        "update\nfixture/behind\n0.18.0\n"
    );
}

#[test]
fn stack_refresh_breaking_verdict_covers_major_feat_bang_and_uninspectable_ranges() {
    let dir = tempdir().unwrap();
    let downstream_path = dir.path().join("downstream");

    // Major bump: breaking from the version shape alone.
    let major_upstream = dir.path().join("upstream-major");
    upstream_repo_with_tags(&major_upstream, &["transformer-v2.0.0"]);
    let mut edge = outdated_edge(
        "fixture/behind",
        Some(homeboy::core::component::DependencyStackEdgeSource {
            repo: major_upstream.display().to_string(),
            prefix: "transformer".to_string(),
        }),
    );
    edge.update = Some("true".to_string());
    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "1.9.0")],
        vec![edge.clone()],
    );
    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, true, true).unwrap();
    let verdict = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(verdict.breaking, Some(true));
    assert!(
        verdict.breaking_reasons[0].contains("major version changes"),
        "unexpected reasons: {:?}",
        verdict.breaking_reasons
    );

    // A patch bump whose range carries a `feat!:` commit is breaking.
    let bang_upstream = dir.path().join("upstream-bang");
    upstream_repo_with_tags(&bang_upstream, &["transformer-v1.0.0"]);
    upstream_commit_with_tags(
        &bang_upstream,
        "feat!: drop legacy API",
        &["transformer-v1.0.1"],
    );
    edge.source = Some(homeboy::core::component::DependencyStackEdgeSource {
        repo: bang_upstream.display().to_string(),
        prefix: "transformer".to_string(),
    });
    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "1.0.0")],
        vec![edge.clone()],
    );
    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, true, true).unwrap();
    let verdict = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(verdict.breaking, Some(true));
    assert!(
        verdict
            .breaking_reasons
            .iter()
            .any(|reason| reason.contains("feat!: drop legacy API")),
        "unexpected reasons: {:?}",
        verdict.breaking_reasons
    );
    assert!(result.breaking);

    // A patch bump whose range carries only non-breaking commits is clean.
    let clean_upstream = dir.path().join("upstream-clean");
    upstream_repo_with_tags(&clean_upstream, &["transformer-v1.0.0"]);
    upstream_commit_with_tags(
        &clean_upstream,
        "fix: tighten parsing",
        &["transformer-v1.0.1"],
    );
    edge.source = Some(homeboy::core::component::DependencyStackEdgeSource {
        repo: clean_upstream.display().to_string(),
        prefix: "transformer".to_string(),
    });
    let component = outdated_downstream(
        &downstream_path,
        &[("fixture/behind", "1.0.0")],
        vec![edge.clone()],
    );
    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, true, true).unwrap();
    let verdict = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(verdict.breaking, Some(false));
    assert!(verdict.breaking_reasons.is_empty());
    assert!(!result.breaking, "a clean range does not mark the run");

    // The locked release was never tagged: the range cannot be inspected and
    // the verdict is null, which callers (and the run) treat as breaking.
    let gapped_upstream = dir.path().join("upstream-gapped");
    upstream_repo_with_tags(&gapped_upstream, &["transformer-v1.0.1"]);
    edge.source = Some(homeboy::core::component::DependencyStackEdgeSource {
        repo: gapped_upstream.display().to_string(),
        prefix: "transformer".to_string(),
    });
    let component =
        outdated_downstream(&downstream_path, &[("fixture/behind", "1.0.0")], vec![edge]);
    let result =
        deps::stack_refresh_for_component(&component, &downstream_path, true, true).unwrap();
    let verdict = refresh_edge_by_package(&result, "fixture/behind");
    assert_eq!(verdict.breaking, None);
    assert!(
        verdict
            .breaking_reasons
            .iter()
            .any(|reason| reason.contains("could not be fetched")),
        "unexpected reasons: {:?}",
        verdict.breaking_reasons
    );
    assert!(
        result.breaking,
        "a null verdict on an updated edge marks the run breaking"
    );
}
