use super::*;
use std::fs;
use std::process::Command;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn bundled_package_builders_declare_workspace_dependency_edges() {
    for manifest in [
        include_str!("../../../../../extensions/nodejs/nodejs.json"),
        include_str!("../../../../../extensions/wordpress/wordpress.json"),
    ] {
        let config: serde_json::Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(
            config["build"]["cache"]["dependencies_setting"],
            "local_workspace_dependencies"
        );
    }
}

#[test]
fn disposable_overlapping_dependencies_build_once_and_reuse_content_cache() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().unwrap();
        let extension = paths::extension("cache-fixture").unwrap();
        fs::create_dir_all(&extension).unwrap();
        fs::write(
            extension.join("cache-fixture.json"),
            serde_json::json!({
                "name":"Cache fixture", "version":"1.0.0",
                "build":{"cache":{"recipe":"fixture-v1","dependencies_setting":"workspace"}}
            })
            .to_string(),
        )
        .unwrap();
        for (id, deps) in [
            ("tokens", serde_json::json!([])),
            ("components", serde_json::json!([{"path":"../tokens"}])),
            (
                "community",
                serde_json::json!([{"path":"../tokens"},{"path":"../components"}]),
            ),
            (
                "studio",
                serde_json::json!([{"path":"../tokens"},{"path":"../components"}]),
            ),
        ] {
            let dir = root.path().join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(".gitignore"), "build/\n").unwrap();
            // A second execution in the same tree fails: this catches duplicate
            // scheduling even when a dirty tree has disabled the cache.
            fs::write(dir.join("homeboy.json"), serde_json::json!({
                "id":id,"build_artifact":"build/package.txt",
                "extensions":{"cache-fixture":{"workspace":deps}},
                "scripts":{"build":["test ! -e build/count && mkdir -p build && echo run > build/count && echo artifact > build/package.txt"]}
            }).to_string()).unwrap();
        }
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        let mut community = component::try_discover_from_portable(&root.path().join("community"))
            .unwrap()
            .unwrap();
        let original_key = cache::content_key(&community).expect("clean content key");
        // Deploy preparation carries a reduced component; a target binding must
        // not move the key away from the one the build stored.
        let bound = Component {
            remote_path: "wp-content/plugins/community".into(),
            remote_url: Some("/srv/checkout".into()),
            changelog_target: Some("docs/CHANGELOG.md".into()),
            ..community.clone()
        };
        assert_eq!(
            cache::content_key(&bound).as_deref(),
            Some(original_key.as_str())
        );
        community
            .env
            .insert("HOMEBOY_BUILD_SOURCE_DISPOSABLE".into(), "1".into());
        assert_eq!(
            cache::content_key(&community).as_deref(),
            Some(original_key.as_str())
        );
        community
            .env
            .insert("CUSTOM_BUILD_INPUT".into(), "1".into());
        assert!(cache::content_key(&community).is_none());
        community.env.remove("CUSTOM_BUILD_INPUT");
        std::env::set_var("HOMEBOY_BUILD_SOURCE_DISPOSABLE", "1");
        for id in ["community", "studio"] {
            let mut comp = component::try_discover_from_portable(&root.path().join(id))
                .unwrap()
                .unwrap();
            comp.env = community.env.clone();
            let (first, code) = execute_build_component(&comp, None).unwrap();
            assert_eq!(code, 0);
            assert!(first.success);
            assert!(first.key.is_some());
            assert_eq!(first.status, "built");
            fs::remove_file(root.path().join(id).join("build/package.txt")).unwrap();
            let (second, code) = execute_build_component(&comp, None).unwrap();
            assert_eq!(code, 0);
            assert_eq!(second.status, "cached");
            assert_eq!(first.key, second.key);
            assert!(root.path().join(id).join("build/package.txt").is_file());
        }
        // Dirty builds must also deduplicate the diamond by canonical path.
        for id in ["tokens", "components", "community"] {
            fs::remove_dir_all(root.path().join(id).join("build")).unwrap();
            fs::write(root.path().join(id).join("untracked"), "dirty").unwrap();
        }
        let (dirty, code) = execute_build_component(&community, None).unwrap();
        assert_eq!(code, 0);
        assert!(dirty.success);
        assert!(dirty.key.is_none());
        // Preserve both output streams and identify a transitive failure.
        fs::remove_file(root.path().join("tokens/build/count")).unwrap();
        let manifest = root.path().join("tokens/homeboy.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        config["scripts"]["build"] =
            serde_json::json!(["echo stdout-diagnostic; echo stderr-diagnostic >&2; exit 42"]);
        fs::write(&manifest, config.to_string()).unwrap();
        let error = execute_build_component(&community, None).unwrap_err();
        for expected in [
            "community",
            "tokens",
            "42",
            "stdout-diagnostic",
            "stderr-diagnostic",
        ] {
            assert!(error.message.contains(expected), "{}", error.message);
        }
        std::env::remove_var("HOMEBOY_BUILD_SOURCE_DISPOSABLE");
    });
}
