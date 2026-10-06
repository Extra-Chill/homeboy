use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const PRIMARY: &str = "zai-coding-plan/glm-5.3-flash";
const ALTERNATIVE: &str = "anthropic/claude-sonnet-5";
const OUTSIDE: &str = "openai/gpt-6-luna";

#[test]
fn cook_preview_uses_cli_ack_policy_for_configured_and_outside_models() {
    let home = tempfile::tempdir().expect("isolated Homeboy home");
    configure_rotation(&home, &[PRIMARY, ALTERNATIVE]);

    let primary = run_preview(&home, PRIMARY, false);
    assert_success(&primary, "configured primary");
    assert_preview_model(&primary, PRIMARY);

    let alternative = run_preview(&home, ALTERNATIVE, false);
    assert_success(&alternative, "configured alternative");
    assert_preview_model(&alternative, ALTERNATIVE);

    let outside = run_preview(&home, OUTSIDE, false);
    assert!(
        !outside.status.success(),
        "unconfigured model requires acknowledgement: stdout={} stderr={}",
        String::from_utf8_lossy(&outside.stdout),
        String::from_utf8_lossy(&outside.stderr)
    );
    let failure = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outside.stdout),
        String::from_utf8_lossy(&outside.stderr)
    );
    assert!(
        failure.contains("explicit acknowledgement is required"),
        "typed override refusal missing: {failure}"
    );

    let acknowledged = run_preview(&home, OUTSIDE, true);
    assert_success(&acknowledged, "acknowledged outside model");
    assert_preview_model(&acknowledged, OUTSIDE);
}

/// Exercise the ordinary CLI -> Cook compile -> runtime provider wrapper ->
/// actual `opencode run` argv boundary. The runtime adapter is the installed
/// OpenCode adapter; only the final OpenCode executable is replaced by a local
/// argv recorder, so no inference or provider credentials are used.
#[cfg(unix)]
#[test]
fn acknowledged_cook_launches_opencode_with_the_concrete_cli_model() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let context = HermeticTestContext::new();
    let home = context.home();
    configure_opencode_rotation(home, &[PRIMARY, ALTERNATIVE]);
    let capacity = Command::new(homeboy_bin())
        .args([
            "config",
            "set",
            "/retention/reconstructable_artifact_reserve_bytes",
            "1",
        ])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("configure isolated capacity for tiny test worktree");
    assert_success(&capacity, "configure isolated test capacity");
    let original_home = PathBuf::from(std::env::var_os("HOME").expect("test HOME"));
    let installed_runtime = original_home.join(".config/homeboy/agent-runtimes/opencode");
    if !installed_runtime.join("opencode.json").is_file() {
        eprintln!("skipping live-adapter CLI launch: installed OpenCode runtime unavailable");
        return;
    }

    let isolated_config = home.join(".config/homeboy");
    let runtime_root = isolated_config.join("agent-runtimes");
    std::fs::create_dir_all(&runtime_root).expect("runtime catalog directory");
    symlink(&installed_runtime, runtime_root.join("opencode")).expect("link installed adapter");
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(installed_runtime.join("opencode.json")).expect("runtime manifest"),
    )
    .expect("runtime manifest JSON");
    for provider in manifest["agent_task_executors"]
        .as_array_mut()
        .expect("runtime executor declarations")
    {
        provider
            .as_object_mut()
            .expect("provider object")
            .remove("readiness_invocation");
        provider["runner_readiness"] = json!([]);
    }
    std::fs::write(
        runtime_root.join("opencode.json"),
        serde_json::to_vec(&manifest).expect("serialize isolated runtime manifest"),
    )
    .expect("write isolated runtime declaration");

    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).expect("fake OpenCode bin directory");
    let capture = std::env::var_os("HOMEBOY_TEST_PROVIDER_ARGV_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("opencode-argv.jsonl"));
    let fake_opencode = bin.join("opencode");
    std::fs::write(
        &fake_opencode,
        format!(
            "#!/usr/bin/env node\nconst fs=require('node:fs');const args=process.argv.slice(2);if(args[0]==='--version'){{process.stdout.write('opencode 1.0.0\\n');process.exit(0);}}if(args[0]==='auth'&&args[1]==='list'){{process.stdout.write('anthropic\\n');process.exit(0);}}if(args[0]==='models'&&args[1]==='anthropic'){{process.stdout.write('anthropic/claude-sonnet-5\\n');process.exit(0);}}if(args[0]==='debug'&&args[1]==='agent'){{const cwd=process.cwd();process.stdout.write(JSON.stringify({{permission:{{external_directory:{{[cwd]:'allow',[cwd+'/**']:'allow'}},read:'allow',glob:'allow',grep:'allow',edit:'allow',bash:'allow'}}}}));process.exit(0);}}if(args[0]==='run'){{fs.appendFileSync({capture:?},JSON.stringify(args)+'\\n');process.exit(0);}}process.exit(0);\n"
        ),
    )
    .expect("write fake OpenCode executable");
    let mut permissions = std::fs::metadata(&fake_opencode)
        .expect("fake executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_opencode, permissions).expect("make fake executable runnable");

    let source = home.join("source");
    initialize_git_repository(&source);
    let workspace = home.join("workspace");
    let linked = Command::new("git")
        .args([
            "worktree",
            "add",
            "--quiet",
            "-b",
            "fix/14835-model-route-test",
            workspace.to_str().expect("workspace path"),
            "main",
        ])
        .current_dir(&source)
        .output()
        .expect("create linked Cook worktree");
    assert!(
        linked.status.success(),
        "git worktree add: {}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let provider_config = home.join("provider-config.json");
    std::fs::write(&provider_config, r#"{"provider":"anthropic"}"#)
        .expect("write file-backed provider configuration");
    let provider_config_ref = format!("@{}", provider_config.display());
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        .args([
            "--wait",
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/14835",
            "--head",
            "fix/14835-model-route-test",
            "--backend",
            "opencode",
            "--selector",
            "opencode.agent-task-executor",
            "--model",
            ALTERNATIVE,
            "--provider-config",
            &provider_config_ref,
            "--acknowledge-model-override",
            "--cwd",
            workspace.to_str().expect("workspace path"),
            "--base",
            "main",
            "--goal",
            "Capture selected concrete model",
            "--prompt",
            "Report the selected model without changing files",
            "--max-attempts",
            "1",
            "--verify",
            "true",
            "--no-finalize",
        ])
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("OPENAI_API_KEY", "test-only-unusable-key")
        .env("PATH", prepend_path(&bin));
    let output = command.output().expect("execute acknowledged Cook CLI");
    let capture = std::fs::read_to_string(&capture).unwrap_or_else(|error| {
        panic!(
            "Cook did not spawn fake OpenCode (status={:?}, error={error}): stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let invocations = capture
        .lines()
        .map(|line| serde_json::from_str::<Vec<String>>(line).expect("captured argv JSON"))
        .collect::<Vec<_>>();
    let run = invocations
        .iter()
        .find(|argv| {
            argv.first().is_some_and(|arg| arg == "run")
                && !argv.windows(2).any(|pair| pair == ["--agent", "homeboy-readiness"])
        })
        .unwrap_or_else(|| {
            panic!(
                "OpenCode run invocation absent; argv={invocations:?}; Cook status={:?}; stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    let model_index = run
        .iter()
        .position(|arg| arg == "--model")
        .expect("--model arg");
    assert_eq!(
        run.get(model_index + 1).map(String::as_str),
        Some(ALTERNATIVE)
    );
}

fn configure_rotation(home: &TempDir, models: &[&str]) {
    configure_rotation_at(home.path(), models);
}

fn configure_rotation_at(home: &Path, models: &[&str]) {
    let entries = models
        .iter()
        .map(|model| json!({ "backend": "fixture", "model": model }))
        .collect::<Vec<_>>();
    let output = Command::new(homeboy_bin())
        .args([
            "config",
            "set",
            "/agent_task/rotation",
            &serde_json::to_string(&json!({ "entries": entries })).expect("rotation JSON"),
        ])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("configure isolated rotation");
    assert_success(&output, "configure isolated model rotation");
    let show = Command::new(homeboy_bin())
        .args(["config", "show", "/agent_task/rotation"])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("read isolated model rotation");
    assert_success(&show, "read isolated model rotation");
    let configured = String::from_utf8_lossy(&show.stdout);
    assert!(
        configured.contains(PRIMARY) && configured.contains(ALTERNATIVE),
        "{configured}"
    );
}

fn configure_opencode_rotation(home: &Path, models: &[&str]) {
    let entries = models
        .iter()
        .map(|model| {
            json!({
                "backend": "opencode",
                "selector": "opencode.agent-task-executor",
                "model": model
            })
        })
        .collect::<Vec<_>>();
    let output = Command::new(homeboy_bin())
        .args([
            "config",
            "set",
            "/agent_task/rotation",
            &serde_json::to_string(&json!({ "entries": entries })).expect("rotation JSON"),
        ])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("configure isolated OpenCode rotation");
    assert_success(&output, "configure isolated OpenCode rotation");
}

fn run_preview(home: &TempDir, model: &str, acknowledged: bool) -> Output {
    let mut command = Command::new(homeboy_bin());
    command.args([
        "agent-task",
        "cook",
        "--repo",
        "homeboy",
        "--task-url",
        "https://github.com/Extra-Chill/homeboy/issues/14835",
        "--head",
        "fix/14835-model-route-preview",
        "--base",
        "HEAD",
        "--backend",
        "fixture",
        "--model",
        model,
        "--prompt",
        "Select the requested configured model",
        "--verify",
        "true",
        "--preview",
        "--no-finalize",
    ]);
    if acknowledged {
        command.arg("--acknowledge-model-override");
    }
    command
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run isolated Cook preview")
}

fn assert_preview_model(output: &Output, model: &str) {
    let preview: Value = serde_json::from_slice(&output.stdout).expect("Cook preview JSON");
    assert_eq!(
        preview["data"]["schema"],
        "homeboy/agent-task-cook-preview/v1"
    );
    assert_eq!(preview["data"]["resolved"]["provider"]["model"], model);
    assert_eq!(
        preview["data"]["resolved"]["provider"]["runtime_selection"]["model"],
        model
    );
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label}: status={:?} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn prepend_path(bin: &Path) -> String {
    std::env::join_paths(
        std::iter::once(bin.to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )
    .expect("compose fake executable PATH")
    .to_string_lossy()
    .into_owned()
}

fn initialize_git_repository(path: &Path) {
    std::fs::create_dir_all(path).expect("workspace directory");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "fixture@example.test"],
        vec!["config", "user.name", "Fixture"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git setup");
        assert!(
            output.status.success(),
            "git setup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(path.join("README.md"), "fixture\n").expect("workspace marker");
    for args in [
        vec!["add", "README.md"],
        vec!["commit", "--quiet", "-m", "fixture"],
        vec!["branch", "-M", "main"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git commit fixture");
        assert!(
            output.status.success(),
            "git setup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
