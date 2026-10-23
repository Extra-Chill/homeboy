use homeboy::core::test_support::{bounded_output, HermeticTestContext, TestBinary};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const PRIMARY: &str = "zai-coding-plan/glm-5.3-flash";
const ALTERNATIVE: &str = "anthropic/claude-sonnet-5";
const OUTSIDE: &str = "openai/gpt-6-luna";
const BLOCKED_MODEL: &str = "blocked-model";
const READY_MODEL: &str = "ready-model";

#[test]
fn cook_preview_uses_cli_ack_policy_for_configured_and_outside_models() {
    let home = tempfile::tempdir().expect("isolated Homeboy home");
    configure_rotation(&home, &[PRIMARY, ALTERNATIVE]);

    let unconfirmed_primary = run_preview(&home, PRIMARY, false);
    assert!(
        !unconfirmed_primary.status.success(),
        "a configured-model pin requires confirmation"
    );
    let primary = run_preview(&home, PRIMARY, true);
    assert_success(&primary, "configured primary");
    assert_preview_model(&primary, PRIMARY);

    let unconfirmed_alternative = run_preview(&home, ALTERNATIVE, false);
    assert!(
        !unconfirmed_alternative.status.success(),
        "an alternative-model pin requires confirmation"
    );
    let alternative = run_preview(&home, ALTERNATIVE, true);
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

/// An unacknowledged configured rotation whose first entry fails provider
/// readiness (credentials rejected) must not let preview name that entry as
/// the model: readiness binds the first trusted entry instead, and the JSON
/// and stderr disclose the bound route, the skipped entries, and their
/// reasons (#15854).
#[test]
fn cook_preview_reports_the_readiness_bound_rotation_route() {
    let home = tempfile::tempdir().expect("isolated Homeboy home");
    let config_root = home.path().join(".config/homeboy");
    let runtime_root = config_root.join("agent-runtimes/proof");
    std::fs::create_dir_all(&runtime_root).expect("runtime catalog directory");
    let readiness = home.path().join("readiness.js");
    std::fs::write(
        &readiness,
        "const fs=require('fs');const input=JSON.parse(fs.readFileSync(0,'utf8'));const model=input.effective_config.model;const ready=model==='ready-model';process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready,classification:ready?'ready':'auth_failure',retryable:false,remediation:'rotate to a configured credential',reason:ready?'':'authentication_rejected',cache_key:String(model),identity:{model}}));",
    )
    .expect("readiness script");
    std::fs::write(
        runtime_root.join("proof.json"),
        serde_json::to_vec(&json!({
            "schema": "homeboy/agent-runtime-manifest/v1",
            "id": "proof",
            "agent_task_executors": [{
                "schema": "homeboy/agent-task-executor-provider/v1",
                "id": "proof.agent-task-executor",
                "backend": "proof",
                "invocation": { "schema": "homeboy/command-invocation/v1", "argv": ["true"] },
                "readiness_invocation": { "argv": [
                    "node",
                    readiness.to_str().expect("readiness path")
                ] }
            }]
        }))
        .expect("runtime manifest"),
    )
    .expect("write runtime declaration");
    // Rotation entries for this runtime are proof-backend entries the shared
    // fixture-backend helper cannot express.
    let result = Command::new(homeboy_bin())
        .args([
            "config",
            "set",
            "/agent_task/rotation",
            &serde_json::to_string(&json!({
                "entries": [
                    { "backend": "proof", "model": BLOCKED_MODEL },
                    { "backend": "proof", "model": READY_MODEL },
                ]
            }))
            .expect("rotation JSON"),
        ])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("configure isolated model rotation");
    assert_success(&result, "configure isolated model rotation");
    let result = Command::new(homeboy_bin())
        .args([
            "config",
            "set",
            "/agent_task/default_backend",
            "proof",
            "--string",
        ])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("configure isolated default backend");
    assert_success(&result, "configure isolated default backend");

    let repo = home.path().join("repo");
    initialize_git_repository(&repo);
    let output = Command::new(homeboy_bin())
        .args([
            "agent-task",
            "cook",
            "--repo",
            repo.to_str().expect("repo path"),
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/15854",
            "--base",
            "HEAD",
            "--prompt",
            "Prove the readiness-bound route projection",
            "--verify",
            "true",
            "--preview",
            "--no-finalize",
        ])
        .current_dir(&repo)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_TEST_LOAD_AVERAGES", "0,0,0")
        .output()
        .expect("run isolated Cook preview");
    assert_success(&output, "readiness rotation preview");

    let preview: Value = serde_json::from_slice(&output.stdout).expect("Cook preview JSON");
    assert_eq!(
        preview["data"]["resolved"]["provider"]["model"],
        READY_MODEL,
        "preview must name the model readiness will bind, stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let readiness_projection = &preview["data"]["resolved"]["provider_readiness"];
    assert_eq!(
        readiness_projection["schema"],
        "homeboy/agent-task-provider-readiness-preview/v1"
    );
    assert_eq!(readiness_projection["binding_owner"], "controller");
    assert_eq!(
        readiness_projection["route_order"],
        json!(["proof/blocked-model", "proof/ready-model"])
    );
    assert_eq!(readiness_projection["selected_route"]["model"], READY_MODEL);
    let skipped = readiness_projection["skipped"]
        .as_array()
        .expect("skipped readiness evidence");
    assert_eq!(skipped[0]["model"], BLOCKED_MODEL);
    assert_eq!(skipped[0]["classification"], "auth_failure");
    assert!(skipped[0]["reason"]
        .as_str()
        .expect("skipped reason")
        .contains("authentication_rejected"));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("provider readiness bound route: proof/ready-model"),
        "stderr must disclose the bound route: {stderr}"
    );
    assert!(
        stderr.contains("(auth_failure: ") && stderr.contains("authentication_rejected"),
        "stderr must disclose skipped entries and reasons: {stderr}"
    );
}

/// Exercise the ordinary CLI -> Cook compile -> runtime provider wrapper ->
/// actual `pi --mode json` argv boundary. The runtime adapter is the installed
/// Pi adapter; only the final Pi executable is replaced by a local
/// argv recorder, so no inference or provider credentials are used.
#[cfg(unix)]
#[test]
fn acknowledged_cook_launches_pi_with_the_concrete_cli_model() {
    use std::os::unix::fs::symlink;

    let context = HermeticTestContext::new();
    let home = context.home();
    configure_pi_rotation(home, &[PRIMARY, ALTERNATIVE]);
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
    let installed_runtime = original_home.join(".config/homeboy/agent-runtimes/pi");
    if !installed_runtime.join("pi.json").is_file() {
        eprintln!("skipping live-adapter CLI launch: installed Pi runtime unavailable");
        return;
    }

    let isolated_config = home.join(".config/homeboy");
    let runtime_root = isolated_config.join("agent-runtimes");
    std::fs::create_dir_all(&runtime_root).expect("runtime catalog directory");
    symlink(&installed_runtime, runtime_root.join("pi")).expect("link installed adapter");
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(installed_runtime.join("pi.json")).expect("runtime manifest"),
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
        runtime_root.join("pi.json"),
        serde_json::to_vec(&manifest).expect("serialize isolated runtime manifest"),
    )
    .expect("write isolated runtime declaration");

    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).expect("fake Pi bin directory");
    let capture = std::env::var_os("HOMEBOY_TEST_PROVIDER_ARGV_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("pi-argv.jsonl"));
    let fake_pi = bin.join("pi");
    homeboy_test_support::write_executable(&fake_pi, format!(
            r#"#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
if (args[0] === '--version') {{
    process.stdout.write('pi 1.0.0\n');
}} else if (args[0] === '--list-models') {{
    process.stdout.write('provider model\nzai-coding-plan glm-5.3-flash\nanthropic claude-sonnet-5\n');
}} else if (args.includes('--mode') && args[args.indexOf('--mode') + 1] === 'json') {{
    fs.appendFileSync({capture:?}, JSON.stringify(args) + '\n');
    process.stdout.write(JSON.stringify({{type: 'agent_end', messages: []}}) + '\n');
}} else {{
    console.error('Unexpected Pi invocation: ' + JSON.stringify(args));
    process.exit(1);
}}
"#
        ), 0o755)
    .expect("write fake Pi executable");

    let source = home.join("source");
    initialize_git_repository(&source);
    // Cook's base preflight resolves `--base main` with `git ls-remote origin`,
    // so the fixture needs a real (local, bare) origin carrying `main`.
    let origin = home.join("origin.git");
    for (dir, args) in [
        (
            home.to_path_buf(),
            vec![
                "init",
                "--quiet",
                "--bare",
                origin.to_str().expect("origin path"),
            ],
        ),
        (
            source.clone(),
            vec![
                "remote",
                "add",
                "origin",
                origin.to_str().expect("origin path"),
            ],
        ),
        (source.clone(), vec!["push", "--quiet", "origin", "main"]),
        (source.clone(), vec!["fetch", "--quiet", "origin"]),
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .output()
            .expect("configure fixture origin");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
            "pi",
            "--selector",
            "pi.agent-task-executor",
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
        // Auto placement consults the host's real load average before it
        // admits local execution. On a loaded CI runner (load >= 0.75 per
        // core while nextest runs process-heavy tests in parallel) local
        // admission is rejected, and with no Lab runner configured the Cook
        // queues as `blocked_runner_unavailable` and retries until the
        // 120s slow-timeout kills it (#15756). This test is about the
        // model argv boundary, not pressure admission, so pin a quiet
        // reading the same way the Lab handoff tests do (#14984).
        .env("HOMEBOY_TEST_LOAD_AVERAGES", "0,0,0")
        .env("OPENAI_API_KEY", "test-only-unusable-key")
        .env("PATH", prepend_path(&bin));
    let output = bounded_output(command);
    let capture = std::fs::read_to_string(&capture).unwrap_or_else(|error| {
        panic!(
            "Cook did not spawn fake Pi (status={:?}, error={error}): stdout={} stderr={}",
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
            argv.windows(2).any(|pair| pair == ["--mode", "json"])
        })
        .unwrap_or_else(|| {
            panic!(
                "Pi run invocation absent; argv={invocations:?}; Cook status={:?}; stdout={} stderr={}",
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
    for model in models {
        assert!(
            configured.contains(model),
            "rotation must configure {model}: {configured}"
        );
    }
}

fn configure_pi_rotation(home: &Path, models: &[&str]) {
    let entries = models
        .iter()
        .map(|model| {
            json!({
                "backend": "pi",
                "selector": "pi.agent-task-executor",
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
        .expect("configure isolated Pi rotation");
    assert_success(&output, "configure isolated Pi rotation");
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
        preview["data"]["resolved"]["retry_budget"]["route"]["fallback"]["mode"],
        "pinned"
    );
    assert_eq!(
        preview["data"]["resolved"]["retry_budget"]["effective"]["max_provider_rotations"],
        0
    );
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
