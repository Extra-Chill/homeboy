use std::{path::Path, process::Command};

#[test]
fn shared_package_build_cache_cli_table() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = temp.path().join("repo");
    let extension = home.join(".config/homeboy/extensions/cache-fixture");
    std::fs::create_dir_all(&extension).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        extension.join("cache-fixture.json"),
        serde_json::json!({
            "name":"Cache fixture", "version":"1.0.0",
            "build": {"cache":{"recipe":"fixture-v1","dependencies_setting":"workspace"}}
        })
        .to_string(),
    )
    .unwrap();
    for id in ["shared", "one", "two"] {
        let dir = repo.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("input"), id).unwrap();
        std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        let deps = if id == "shared" {
            serde_json::json!([])
        } else {
            serde_json::json!([{"path":"../shared"}])
        };
        let build = if id == "shared" {
            "mkdir -p build && printf run >> build/count && cat input > build/package.txt"
        } else {
            "mkdir -p build && printf run >> build/count && cat input ../shared/build/package.txt > build/package.txt"
        };
        std::fs::write(
            dir.join("homeboy.json"),
            serde_json::json!({
                "id":id,"build_artifact":"build/package.txt",
                "extensions":{"cache-fixture":{"workspace":deps}},
                "scripts":{"build":[build]}
            })
            .to_string(),
        )
        .unwrap();
    }
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "fixture"]);
    // Exercise the public project + component selection path.
    cli(
        &home,
        &repo,
        &[
            "server",
            "create",
            "local",
            "--host",
            "localhost",
            "--user",
            "test",
        ],
    );
    cli(
        &home,
        &repo,
        &[
            "project",
            "create",
            "fixture",
            "--server-id",
            "local",
            "--base-path",
            repo.to_str().unwrap(),
        ],
    );
    for id in ["shared", "one", "two"] {
        cli(
            &home,
            &repo,
            &[
                "project",
                "components",
                "attach-path",
                "fixture",
                repo.join(id).to_str().unwrap(),
            ],
        );
    }
    for (case, expected) in [
        ("first", "built"),
        ("repeat", "cached"),
        ("unrelated-commit", "cached"),
        ("shared-commit", "built"),
        ("dirty", "built"),
        ("untracked", "built"),
    ] {
        if case == "unrelated-commit" {
            std::fs::write(repo.join("unrelated"), "not a build input").unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-qm", "unrelated change"]);
        } else if case == "shared-commit" {
            std::fs::write(repo.join("shared/input"), "changed").unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-qm", "shared change"]);
        } else if case == "dirty" {
            std::fs::write(repo.join("shared/input"), "dirty").unwrap();
        } else if case == "untracked" {
            git(&repo, &["checkout", "--", "shared/input"]);
            std::fs::write(repo.join("shared/untracked"), "new input").unwrap();
        }
        for id in ["shared", "one", "two"] {
            let _ = std::fs::remove_dir_all(repo.join(id).join("build"));
        }
        let out = temp.path().join(format!("{case}.json"));
        cli(
            &home,
            &repo,
            &[
                "review",
                "build",
                "fixture",
                "--all",
                "--output",
                out.to_str().unwrap(),
            ],
        );
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap();
        let text = value.to_string();
        assert_eq!(
            text.matches(&format!("\"status\":\"{expected}\"")).count(),
            3,
            "{case}: {value}"
        );
        for id in ["shared", "one", "two"] {
            assert!(
                repo.join(id).join("build/package.txt").is_file(),
                "{case}: {id}"
            );
        }
        for id in ["shared", "one", "two"] {
            let count =
                std::fs::read_to_string(repo.join(id).join("build/count")).unwrap_or_default();
            assert_eq!(
                count,
                if expected == "built" { "run" } else { "" },
                "{case}: {id} recipe executions"
            );
        }
        eprintln!("build-cache e2e: {case}: shared, one, two = {expected}");
    }
}

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

fn cli(home: &Path, root: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_homeboy"))
        .args(["--placement", "local"])
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOMEBOY_DATA_DIR")
        .env("HOMEBOY_BUILD_CACHE_DIR", home.join("cache"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
