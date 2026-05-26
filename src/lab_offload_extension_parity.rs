use std::collections::BTreeSet;

use homeboy::cli_surface::Commands;

pub struct LabOffloadCapabilityPlanPreflight {
    _private: (),
}

pub fn after_capability_plan() -> LabOffloadCapabilityPlanPreflight {
    LabOffloadCapabilityPlanPreflight { _private: () }
}

pub fn preflight(
    command: &Commands,
    runner_id: &str,
    homeboy_path: &str,
    remote_cwd: &str,
    _capability_plan_preflight: LabOffloadCapabilityPlanPreflight,
) -> homeboy::core::Result<()> {
    let extension_ids = parity_extension_ids(command)?;
    for extension_id in extension_ids {
        let (output, exit_code) = homeboy::core::runner::exec(
            runner_id,
            homeboy::core::runner::RunnerExecOptions {
                cwd: Some(remote_cwd.to_string()),
                allow_ssh: false,
                command: vec![
                    homeboy_path.to_string(),
                    "extension".to_string(),
                    "show".to_string(),
                    extension_id.clone(),
                ],
                env: Default::default(),
                capture_patch: false,
                source_snapshot: None,
            },
        )?;

        if exit_code != 0 {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "runner_extension",
                format!(
                    "Lab offload runner '{runner_id}' is missing required extension '{extension_id}' before command execution"
                ),
                Some(extension_id.clone()),
                Some(vec![
                    format!(
                        "Install the extension on the runner before offloading this command: {homeboy_path} extension install <source> --id {extension_id}"
                    ),
                    format!(
                        "Remote preflight command failed: {homeboy_path} extension show {extension_id}"
                    ),
                    preflight_tail(&output.stderr, &output.stdout),
                ]),
            ));
        }
    }

    Ok(())
}

fn parity_extension_ids(command: &Commands) -> homeboy::core::Result<Vec<String>> {
    let mut extension_ids = BTreeSet::new();

    match command {
        Commands::Audit(args) => extension_ids.extend(args.extension_override.extensions.clone()),
        Commands::Bench(args) => {
            extension_ids.extend(args.extension_override_ids().iter().cloned())
        }
        Commands::Lint(args) => extension_ids.extend(args.extension_override.extensions.clone()),
        Commands::Test(args) => {
            extension_ids.extend(args.extension_override.extensions.clone());
            extension_ids.extend(test_extension_ids(command)?);
        }
        _ => {}
    }

    Ok(extension_ids.into_iter().collect())
}

fn test_extension_ids(command: &Commands) -> homeboy::core::Result<Vec<String>> {
    let Commands::Test(args) = command else {
        return Ok(Vec::new());
    };

    let source_context = homeboy::core::engine::execution_context::resolve(
        &homeboy::core::engine::execution_context::ResolveOptions {
            component_id: args.comp.component.clone(),
            path_override: args.comp.path.clone(),
            capability: None,
            settings_overrides: args.setting_args.setting.clone(),
            settings_json_overrides: args.setting_args.setting_json.clone(),
            extension_overrides: args.extension_override.extensions.clone(),
        },
    )?;

    if !args.drift
        && args.ci_job.is_none()
        && source_context
            .component
            .has_script(homeboy::core::extension::ExtensionCapability::Test)
    {
        return Ok(Vec::new());
    }

    let context = homeboy::core::engine::execution_context::resolve(
        &homeboy::core::engine::execution_context::ResolveOptions {
            component_id: args.comp.component.clone(),
            path_override: args.comp.path.clone(),
            capability: Some(homeboy::core::extension::ExtensionCapability::Test),
            settings_overrides: args.setting_args.setting.clone(),
            settings_json_overrides: args.setting_args.setting_json.clone(),
            extension_overrides: args.extension_override.extensions.clone(),
        },
    )?;

    Ok(context.extension_id.into_iter().collect())
}

fn preflight_tail(stderr: &str, stdout: &str) -> String {
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let tail = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");

    if tail.is_empty() {
        "Runner extension preflight produced no diagnostic output.".to_string()
    } else {
        format!("Runner extension preflight output:\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::{parity_extension_ids, preflight_tail, test_extension_ids};
    use clap::Parser;
    use homeboy::cli_surface::Commands;
    use homeboy::commands::test::TestArgs;
    use homeboy::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs,
    };
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_args_for_path(path: &std::path::Path) -> TestArgs {
        TestArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: Some(path.to_string_lossy().to_string()),
            },
            extension_override: ExtensionOverrideArgs::default(),
            skip_lint: false,
            coverage: false,
            coverage_min: None,
            baseline_args: BaselineArgs::default(),
            analyze: false,
            drift: false,
            write: false,
            since: "HEAD~10".to_string(),
            changed_since: None,
            ci_job: None,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            _json: HiddenJsonArgs::default(),
            json_summary: false,
        }
    }

    fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = env_lock().lock().expect("env lock");
        let previous_home = std::env::var("HOME").ok();
        let home = tempfile::tempdir().expect("temp home");
        std::env::set_var("HOME", home.path());
        let result = f(home.path());
        if let Some(previous_home) = previous_home {
            std::env::set_var("HOME", previous_home);
        } else {
            std::env::remove_var("HOME");
        }
        result
    }

    #[test]
    fn test_preflight_skips_component_script_components() {
        with_temp_home(|_| {
            let dir = tempfile::tempdir().expect("component dir");
            std::fs::write(
                dir.path().join("homeboy.json"),
                r#"{"id":"fixture","scripts":{"test":["printf ok\n"]},"extensions":{"missing-runner-extension":{}}}"#,
            )
            .expect("write component config");

            let ids = test_extension_ids(&Commands::Test(test_args_for_path(dir.path())))
                .expect("component script should not require extension parity preflight");

            assert!(ids.is_empty());
        });
    }

    #[test]
    fn test_preflight_resolves_selected_test_extension() {
        with_temp_home(|home| {
            write_test_extension(home, "fixture-extension");

            let dir = tempfile::tempdir().expect("component dir");
            std::fs::write(
                dir.path().join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"fixture-extension":{}}}"#,
            )
            .expect("write component config");

            let ids = test_extension_ids(&Commands::Test(test_args_for_path(dir.path())))
                .expect("test extension should resolve locally before runner parity check");

            assert_eq!(ids, vec!["fixture-extension".to_string()]);
        });
    }

    #[test]
    fn parity_includes_lint_selector() {
        let command = homeboy::cli_surface::Cli::try_parse_from([
            "homeboy",
            "lint",
            "--extension",
            "nodejs",
            "--path",
            "/tmp/project",
        ])
        .expect("parse")
        .command;

        let ids = parity_extension_ids(&command).expect("extension ids");

        assert_eq!(ids, vec!["nodejs".to_string()]);
    }

    #[test]
    fn parity_includes_bench_selector() {
        let command = homeboy::cli_surface::Cli::try_parse_from([
            "homeboy",
            "bench",
            "--extension",
            "wordpress",
            "--path",
            "/tmp/project",
        ])
        .expect("parse")
        .command;

        let ids = parity_extension_ids(&command).expect("extension ids");

        assert_eq!(ids, vec!["wordpress".to_string()]);
    }

    #[test]
    fn parity_deduplicates_test_selector_and_resolved_extension() {
        with_temp_home(|home| {
            write_test_extension(home, "fixture-extension");

            let dir = tempfile::tempdir().expect("component dir");
            std::fs::write(
                dir.path().join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"fixture-extension":{}}}"#,
            )
            .expect("write component config");

            let mut args = test_args_for_path(dir.path());
            args.extension_override.extensions = vec!["fixture-extension".to_string()];

            let ids =
                parity_extension_ids(&Commands::Test(args)).expect("extension ids should dedupe");

            assert_eq!(ids, vec!["fixture-extension".to_string()]);
        });
    }

    #[test]
    fn tail_prefers_recent_diagnostics() {
        let tail = preflight_tail(
            "one\ntwo\nthree\nfour",
            "stdout should be ignored when stderr has enough lines",
        );

        assert!(tail.contains("two\nthree\nfour"));
        assert!(!tail.contains("one"));
    }

    fn write_test_extension(home: &std::path::Path, extension_id: &str) {
        let extension_dir = home
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join(extension_id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{extension_id}.json")),
            r#"{"name":"Fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
        )
        .expect("write extension manifest");
    }
}
