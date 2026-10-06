use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use homeboy_core::test_support::{
    bounded_output, git_command_output, run_git_fixture_command, shared_committed_git_repo_fixture,
    HermeticTestContext, TestBinary,
};

fn shell_command(context: &HermeticTestContext, path: &Path, bash_env: &Path) -> Command {
    let mut command = Command::new("/bin/bash");
    command
        .env("HOME", context.home())
        .env("XDG_CONFIG_HOME", context.root().join(".config"))
        .env("XDG_DATA_HOME", context.root().join("data"))
        .env("HOMEBOY_DATA_DIR", context.data_dir())
        .env("HOMEBOY_ARTIFACT_ROOT", context.artifact_dir())
        .env("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir())
        .env("TMPDIR", context.temp_dir())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env(
            "HOMEBOY_COMMAND",
            context.binary_path(TestBinary::HomeboyFixture),
        )
        .env("BASH_ENV", bash_env)
        .env(
            "PATH",
            format!(
                "{}:{}",
                path.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    command
}

fn assert_worktree_owner(worktree: &Path, repository: &Path) {
    assert!(worktree.is_dir(), "worktree exists: {}", worktree.display());
    let common = git_command_output(worktree, &["rev-parse", "--git-common-dir"]);
    assert_eq!(
        std::fs::canonicalize(worktree.join(common)).expect("canonical Git common dir"),
        std::fs::canonicalize(repository.join(".git")).expect("canonical repository store")
    );
}

fn worktree_path(repository: &Path, branch: &str) -> PathBuf {
    repository
        .parent()
        .expect("repository parent")
        .join(format!("runtime-shell-component@{branch}"))
}

fn checked(output: Output, invocation: &str) {
    assert!(
        output.status.success(),
        "{invocation}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn actual_homeboy_runtime_keeps_script_and_login_shell_worktrees_on_registered_repo() {
    use std::os::unix::fs::PermissionsExt;

    let context = HermeticTestContext::new();
    let (_checkout_guard, checkout) = shared_committed_git_repo_fixture("runtime-shell-component");
    let repository = checkout.as_path();
    run_git_fixture_command(repository, &["config", "user.name", "Runtime Test"]);
    run_git_fixture_command(
        repository,
        &["config", "user.email", "runtime@example.test"],
    );
    std::fs::write(
        repository.join("homeboy.json"),
        r#"{"id":"runtime-shell-component"}"#,
    )
    .expect("write component manifest");
    run_git_fixture_command(repository, &["add", "homeboy.json"]);
    run_git_fixture_command(repository, &["commit", "-qm", "component manifest"]);

    let mut register = context.command(TestBinary::HomeboyFixture);
    register.args([
        "component",
        "create",
        "--local-path",
        repository.to_str().expect("repository path"),
    ]);
    checked(bounded_output(register), "register component");

    let tools = context.root().join("competing-bin");
    std::fs::create_dir_all(&tools).expect("competing bin");
    let decoy = tools.join("homeboy");
    std::fs::write(&decoy, "#!/bin/sh\nprintf decoy >&2\nexit 91\n")
        .expect("write competing executable");
    std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755))
        .expect("make competing executable");
    std::fs::write(
        context.home().join(".bash_profile"),
        format!("export PATH='{}:/usr/bin:/bin'\n", tools.display()),
    )
    .expect("write competing login PATH");
    let bash_env = context.runtime_dir().join("runner-bash-env.sh");
    let runtime = context.binary_path(TestBinary::HomeboyFixture);
    std::fs::write(
        &bash_env,
        format!(
            "export PATH='{}':\"$PATH\"\n",
            runtime.parent().unwrap().display()
        ),
    )
    .expect("write runner Bash environment contract");

    let branches = ["runtime-direct", "runtime-script", "runtime-login"];
    let mut direct = context.command(TestBinary::HomeboyFixture);
    direct.current_dir(repository).args([
        "worktree",
        "create",
        "runtime-shell-component",
        "--branch",
        branches[0],
        "--from",
        "HEAD",
    ]);
    checked(bounded_output(direct), "direct argv");

    let script = context.root().join("create-worktree.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\nexec homeboy worktree create runtime-shell-component --branch {} --from HEAD\n",
            branches[1]
        ),
    )
    .expect("write Bash script");
    let mut script_command = shell_command(&context, &tools, &bash_env);
    script_command
        .current_dir(repository)
        .args([script.to_str().unwrap()]);
    checked(bounded_output(script_command), "script-file invocation");

    let mut login = shell_command(&context, &tools, &bash_env);
    login.current_dir(repository).args([
        "-lc",
        &format!(
            "exec homeboy worktree create runtime-shell-component --branch {} --from HEAD",
            branches[2]
        ),
    ]);
    checked(bounded_output(login), "bash -lc invocation");

    for branch in branches {
        assert_worktree_owner(&worktree_path(repository, branch), repository);
    }
}
