use clap::{Args, Subcommand};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use sha2::{Digest, Sha256};

use homeboy::agents::agent_task_scheduler::AgentTaskCandidateCompletionPolicy;
use homeboy::agents::agent_tasks::gate::{
    AgentTaskGateEnvironmentMode, AgentTaskGateEnvironmentPolicy, AgentTaskGateExecutionPolicy,
    AgentTaskGateExtensionInput, AgentTaskGateInputSource, AgentTaskGatePackageArtifactRequirement,
    AgentTaskGateRevealPolicy, AgentTaskGateToolchainRequirement, AgentTaskGateVisibility,
    VerifyGateOptions,
};

use super::super::super::super::agent_task_dispatch::DispatchArgs;
use super::super::super::review;

/// A bounded, controller-projected input available to the provider as read-only
/// workspace evidence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderEvidenceInput {
    pub id: String,
    pub source: String,
}

pub(crate) const PROVIDER_EVIDENCE_DECLARATION: &str = "JSON object with required `id` (unique, non-empty path-free name) and `source` (unique absolute regular-file path): `--provider-evidence '{\"id\":\"evidence\",\"source\":\"/absolute/path\"}'`. Each source is limited to 64 MiB.";

#[derive(Args, Debug, Clone)]
pub struct VerifyGateArgs {
    /// Deterministic verification command that must pass before the cook
    /// promotes its work (e.g. `--verify "cargo fmt --check"`). Required unless
    /// `--private-verify` is given — a cook that cannot verify its work cannot
    /// promote it. Runs in the destination worktree. Repeat to require multiple
    /// gates; every one must pass. Its output is included in the review evidence.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,
    /// Read one public verification shell program from a file. Prefer this for
    /// loops, quotes, multiline programs, or `$variables`; Homeboy snapshots the
    /// exact file bytes before submission. Relative paths use the controller's
    /// invocation directory. Example: `--verify-file quality-gate.sh` containing
    /// `for file in src/*.rs; do cargo fmt --check -- "$file"; done`.
    #[arg(long = "verify-file", value_name = "PATH")]
    pub verify_file: Vec<String>,
    /// Like `--verify`, but the command's output is treated as private: only a
    /// pass/fail summary is revealed by default (see `--private-gate-reveal`).
    /// Satisfies the same mandatory-gate requirement as `--verify`. Use for
    /// gates whose logs may contain secrets. Repeatable.
    #[arg(long = "private-verify", value_name = "COMMAND")]
    pub private_verify: Vec<String>,
    /// Read one private verification shell program from a file. The controller
    /// snapshots its bytes before submission; durable provenance records its
    /// digest and redaction policy, not its file path. Relative paths use the
    /// controller's invocation directory.
    #[arg(long = "private-verify-file", value_name = "PATH")]
    pub private_verify_file: Vec<String>,
    /// Durable source metadata emitted by Homeboy-generated promotion commands.
    /// This preserves the immutable provenance of a previously snapshotted gate;
    /// private entries retain no source path.
    #[arg(long = "gate-input-source", value_name = "JSON", value_parser = parse_gate_input_source)]
    pub input_sources: Vec<AgentTaskGateInputSource>,
    /// How much of a `--private-verify` gate's output to reveal: `summary-only`
    /// (default) shows just pass/fail; other policies expose more detail.
    #[arg(
        long = "private-gate-reveal",
        default_value = "summary-only",
        value_name = "POLICY"
    )]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
    /// Gate scheduling policy: `ordered-fail-fast` (default) skips downstream
    /// gates after the first failure; `continue-all` runs every declared gate.
    #[arg(
        long = "gate-execution-policy",
        default_value = "ordered-fail-fast",
        value_name = "POLICY"
    )]
    #[arg(value_parser = ["ordered-fail-fast", "continue-all"])]
    pub gate_execution_policy: String,
    /// Wall-clock timeout, in seconds, for each verification gate command
    /// (default 1800 = 30 min). A gate exceeding this fails.
    #[arg(long = "gate-timeout-seconds", default_value_t = 30 * 60, value_name = "SECONDS")]
    pub gate_timeout_seconds: u64,
    /// How often, in seconds, to emit a heartbeat while a gate runs so long
    /// gates are not mistaken for a stalled cook (default 5).
    #[arg(
        long = "gate-heartbeat-interval-seconds",
        default_value_t = 5,
        value_name = "SECONDS"
    )]
    pub gate_heartbeat_interval_seconds: u64,
    /// Maximum time, in seconds, a gate may run without a structured
    /// `HOMEBOY_PROGRESS` marker (default 300 = 5 min).
    #[arg(
        long = "gate-no-progress-timeout-seconds",
        default_value_t = 5 * 60,
        value_name = "SECONDS"
    )]
    pub gate_no_progress_timeout_seconds: u64,
    /// Re-run gates that already recorded a passing result on a previous
    /// attempt instead of reusing the recorded pass. Off by default.
    #[arg(long = "rerun-completed-gates")]
    pub rerun_completed_gates: bool,
    /// Finalize only when an inherited required-gate failure was reproduced on
    /// the immutable baseline. The gate remains reported as baseline-red.
    #[arg(long = "accept-inherited-failures")]
    pub accept_inherited_failures: bool,
    /// Environment for gate commands: `inherit` (default) extends the current
    /// environment; `replace` starts from an empty environment plus `--gate-env`.
    #[arg(
        long = "gate-environment-mode",
        default_value = "inherit",
        value_name = "MODE"
    )]
    #[arg(value_parser = ["inherit", "replace"])]
    pub gate_environment_mode: String,
    /// Extra environment variable for gate commands, as `NAME=VALUE`. Repeatable.
    #[arg(long = "gate-env", value_name = "NAME=VALUE", value_parser = parse_gate_environment)]
    pub gate_environment: Vec<(String, String)>,
    /// Preserve a required toolchain setting from the host as `NAME=SOURCE` or
    /// `NAME=SOURCE/relative/path`. The mapping is retained in gate evidence.
    #[arg(long = "gate-env-from", value_name = "NAME=SOURCE[/PATH]", value_parser = parse_gate_environment)]
    pub gate_environment_preserve: Vec<(String, String)>,
    /// Required executable to initialize before provider execution. Its probe is
    /// `COMMAND --version` in the final isolated gate environment. Repeatable.
    #[arg(long = "gate-toolchain", value_name = "COMMAND")]
    pub gate_toolchains: Vec<String>,
    /// Exact toolchain probe contract as JSON. Use when a probe needs arguments
    /// other than the `--version` default retained by `--gate-toolchain`.
    #[arg(long = "gate-toolchain-spec", value_name = "JSON", value_parser = parse_gate_toolchain_requirement)]
    pub gate_toolchain_specs: Vec<AgentTaskGateToolchainRequirement>,
    /// Caller-declared package resource readiness as a JSON object. The object
    /// defines its environment mapping, required paths or digests, and opaque
    /// remediation metadata. Repeat for multiple resources.
    #[arg(long = "gate-package-artifact", value_name = "JSON", value_parser = parse_gate_package_artifact)]
    pub gate_package_artifacts: Vec<AgentTaskGatePackageArtifactRequirement>,
    /// Explicit extension input as a JSON object with `id` and absolute
    /// `source`. Only selected inputs are copied into isolated HOME.
    #[arg(long = "gate-extension-input", value_name = "JSON", value_parser = parse_gate_extension_input)]
    pub gate_extension_inputs: Vec<AgentTaskGateExtensionInput>,
    /// Run gates with an isolated `$HOME` so gate side effects do not touch the
    /// operator's home directory (default true).
    #[arg(
        long = "isolate-gate-home",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub isolate_gate_home: bool,
    /// Run gates with isolated XDG base directories so gate side effects do not
    /// touch the operator's config/cache/data dirs (default true).
    #[arg(
        long = "isolate-gate-xdg",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub isolate_gate_xdg: bool,
    /// Override the component's declared shared Cargo target policy for
    /// deterministic gates. Omit to inherit the repository component policy.
    #[arg(long = "gate-shared-cargo-target", action = clap::ArgAction::SetTrue, conflicts_with = "no_gate_shared_cargo_target")]
    pub gate_shared_cargo_target: bool,
    /// Explicitly keep deterministic gate Cargo output local to its workspace.
    #[arg(long = "no-gate-shared-cargo-target", action = clap::ArgAction::SetTrue)]
    pub no_gate_shared_cargo_target: bool,
}
impl VerifyGateArgs {
    pub(crate) fn has_deterministic_gate(&self) -> bool {
        !self.verify.is_empty()
            || !self.verify_file.is_empty()
            || !self.private_verify.is_empty()
            || !self.private_verify_file.is_empty()
    }

    /// Resolve file inputs while the controller invocation directory still owns
    /// relative-path semantics, before Cook can provision or dispatch anything.
    pub(crate) fn snapshot_file_inputs(&mut self) -> homeboy::core::Result<()> {
        if !self.input_sources.is_empty() {
            if !self.verify_file.is_empty() || !self.private_verify_file.is_empty() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "gate-input-source",
                    "cannot combine retained gate provenance with file inputs",
                    None,
                    Some(vec!["Use inline --verify or --private-verify commands with --gate-input-source, or let Homeboy snapshot the file inputs.".to_string()]),
                ));
            }
            return Ok(());
        }
        self.input_sources.extend(self.verify.iter().map(|program| {
            inline_gate_source(
                program,
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            )
        }));
        self.input_sources
            .extend(self.private_verify.iter().map(|program| {
                inline_gate_source(
                    program,
                    AgentTaskGateVisibility::Private,
                    self.private_gate_reveal,
                )
            }));
        let public_files = std::mem::take(&mut self.verify_file);
        for path in public_files {
            let (program, source) = snapshot_gate_file(
                &path,
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            )?;
            self.verify.push(program);
            self.input_sources.push(source);
        }
        let private_files = std::mem::take(&mut self.private_verify_file);
        for path in private_files {
            let (program, source) = snapshot_gate_file(
                &path,
                AgentTaskGateVisibility::Private,
                self.private_gate_reveal,
            )?;
            self.private_verify.push(program);
            self.input_sources.push(source);
        }
        Ok(())
    }
}

fn inline_gate_source(
    program: &str,
    visibility: AgentTaskGateVisibility,
    redaction_policy: AgentTaskGateRevealPolicy,
) -> AgentTaskGateInputSource {
    AgentTaskGateInputSource {
        visibility,
        source_kind: "inline".to_string(),
        path: None,
        sha256: format!("sha256:{:x}", Sha256::digest(program.as_bytes())),
        size_bytes: program.len() as u64,
        redaction_policy,
    }
}

const MAX_GATE_FILE_BYTES: u64 = 1024 * 1024;

fn snapshot_gate_file(
    input: &str,
    visibility: AgentTaskGateVisibility,
    redaction_policy: AgentTaskGateRevealPolicy,
) -> homeboy::core::Result<(String, AgentTaskGateInputSource)> {
    let path = Path::new(input);
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("cannot read `{input}`: {error}"),
            Some(input.to_string()),
            Some(vec!["Pass a readable shell-program file relative to the controller invocation directory, or use an inline --verify command.".to_string()]),
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("`{input}` is not a regular file"),
            Some(input.to_string()),
            None,
        ));
    }
    if metadata.len() == 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("`{input}` is empty; provide one shell program"),
            Some(input.to_string()),
            None,
        ));
    }
    if metadata.len() > MAX_GATE_FILE_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("`{input}` exceeds the {MAX_GATE_FILE_BYTES}-byte limit"),
            Some(input.to_string()),
            None,
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("cannot safely open `{input}`: {error}"),
            Some(input.to_string()),
            None,
        )
    })?;
    let opened = file.metadata().map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("cannot inspect `{input}` after opening: {error}"),
            Some(input.to_string()),
            None,
        )
    })?;
    #[cfg(unix)]
    let same_identity = metadata.dev() == opened.dev() && metadata.ino() == opened.ino();
    #[cfg(not(unix))]
    // Platforms without a portable device/inode API still validate the same
    // opened descriptor as a bounded regular file. Unix adds identity pinning.
    let same_identity = true;
    if !opened.is_file() || !same_identity || opened.len() > MAX_GATE_FILE_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("`{input}` changed to an unsafe file while opening"),
            Some(input.to_string()),
            None,
        ));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(MAX_GATE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "verification gate file",
                format!("cannot read `{input}`: {error}"),
                Some(input.to_string()),
                None,
            )
        })?;
    if bytes.len() as u64 > MAX_GATE_FILE_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("`{input}` exceeds the {MAX_GATE_FILE_BYTES}-byte limit"),
            Some(input.to_string()),
            None,
        ));
    }
    let program = String::from_utf8(bytes.clone()).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "verification gate file",
            format!("`{input}` is not valid UTF-8 shell text: {error}"),
            Some(input.to_string()),
            None,
        )
    })?;
    let sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
    Ok((
        program,
        AgentTaskGateInputSource {
            visibility,
            source_kind: "file".to_string(),
            path: (visibility == AgentTaskGateVisibility::Visible).then(|| input.to_string()),
            sha256,
            size_bytes: bytes.len() as u64,
            redaction_policy,
        },
    ))
}
impl From<VerifyGateArgs> for VerifyGateOptions {
    fn from(args: VerifyGateArgs) -> Self {
        Self {
            verify: args.verify,
            private_verify: args.private_verify,
            input_sources: args.input_sources,
            private_gate_reveal: args.private_gate_reveal,
            execution_policy: match args.gate_execution_policy.as_str() {
                "continue-all" => AgentTaskGateExecutionPolicy::ContinueAll,
                _ => AgentTaskGateExecutionPolicy::OrderedFailFast,
            },
            gate_timeout_seconds: args.gate_timeout_seconds,
            gate_heartbeat_interval_seconds: args.gate_heartbeat_interval_seconds,
            gate_no_progress_timeout_seconds: args.gate_no_progress_timeout_seconds,
            rerun_completed_gates: args.rerun_completed_gates,
            accept_inherited_failures: args.accept_inherited_failures,
            gate_environment: AgentTaskGateEnvironmentPolicy {
                mode: match args.gate_environment_mode.as_str() {
                    "replace" => AgentTaskGateEnvironmentMode::Replace,
                    _ => AgentTaskGateEnvironmentMode::Inherit,
                },
                variables: args
                    .gate_environment
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
                preserve: args
                    .gate_environment_preserve
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
                isolate_home: args.isolate_gate_home,
                isolate_xdg: args.isolate_gate_xdg,
                hydrate_rust_cache: true,
                shared_cargo_target: args
                    .gate_shared_cargo_target
                    .then_some(true)
                    .or_else(|| args.no_gate_shared_cargo_target.then_some(false)),
                extension_inputs: args.gate_extension_inputs,
            },
            gate_toolchains: args
                .gate_toolchains
                .into_iter()
                .map(|command| AgentTaskGateToolchainRequirement {
                    command,
                    probe_arguments: vec!["--version".to_string()],
                })
                .chain(args.gate_toolchain_specs)
                .collect(),
            gate_package_artifacts: args.gate_package_artifacts,
            gate_diagnostic_sidecars: Vec::new(),
            hydrate_dependencies: true,
        }
    }
}

fn parse_gate_package_artifact(
    value: &str,
) -> Result<AgentTaskGatePackageArtifactRequirement, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("invalid gate package artifact declaration: {error}"))
}

fn parse_gate_input_source(value: &str) -> Result<AgentTaskGateInputSource, String> {
    let source: AgentTaskGateInputSource = serde_json::from_str(value)
        .map_err(|error| format!("invalid gate input source declaration: {error}"))?;
    if source.visibility == AgentTaskGateVisibility::Private && source.path.is_some() {
        return Err("private gate input source must not include a path".to_string());
    }
    Ok(source)
}

fn parse_gate_toolchain_requirement(
    value: &str,
) -> Result<AgentTaskGateToolchainRequirement, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("invalid gate toolchain requirement: {error}"))
}

fn parse_gate_extension_input(value: &str) -> Result<AgentTaskGateExtensionInput, String> {
    serde_json::from_str(value)
        .map_err(|error| format!("invalid gate extension input declaration: {error}"))
}

fn parse_gate_environment(value: &str) -> Result<(String, String), String> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| "expected NAME=VALUE".to_string())?;
    if name.is_empty() || name.contains('=') {
        return Err("environment variable name must not be empty or contain '='".to_string());
    }
    Ok((name.to_string(), value.to_string()))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        gates: VerifyGateArgs,
    }

    #[test]
    fn gate_policy_cli_defaults_and_overrides_round_trip_to_typed_options() {
        let defaults = TestCli::try_parse_from(["homeboy"])
            .expect("parse default gate policy")
            .gates;
        let defaults: VerifyGateOptions = defaults.into();
        assert_eq!(defaults.gate_timeout_seconds, 30 * 60);
        assert_eq!(defaults.gate_heartbeat_interval_seconds, 5);
        assert_eq!(defaults.gate_no_progress_timeout_seconds, 5 * 60);
        assert!(!defaults.rerun_completed_gates);
        assert!(defaults.hydrate_dependencies);

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-timeout-seconds",
            "42",
            "--gate-heartbeat-interval-seconds",
            "7",
            "--gate-no-progress-timeout-seconds",
            "11",
            "--rerun-completed-gates",
        ])
        .expect("parse configured gate policy")
        .gates
        .into();
        assert_eq!(options.gate_timeout_seconds, 42);
        assert_eq!(options.gate_heartbeat_interval_seconds, 7);
        assert_eq!(options.gate_no_progress_timeout_seconds, 11);
        assert!(options.rerun_completed_gates);
        assert!(options.gate_environment.isolate_home);
        assert!(options.gate_environment.isolate_xdg);

        let options: VerifyGateOptions =
            TestCli::try_parse_from(["homeboy", "--gate-execution-policy", "continue-all"])
                .expect("parse continue-all gate policy")
                .gates
                .into();
        assert_eq!(
            options.execution_policy,
            AgentTaskGateExecutionPolicy::ContinueAll
        );

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-environment-mode",
            "replace",
            "--gate-env",
            "FEATURE=enabled",
            "--gate-extension-input",
            r#"{"id":"wordpress","source":"/opt/extensions/wordpress","identity":"sha256:content"}"#,
        ])
        .expect("parse gate environment")
        .gates
        .into();
        assert_eq!(
            options.gate_environment.mode,
            AgentTaskGateEnvironmentMode::Replace
        );
        assert_eq!(options.gate_environment.variables["FEATURE"], "enabled");
        assert_eq!(
            options.gate_environment.extension_inputs,
            vec![AgentTaskGateExtensionInput {
                id: "wordpress".to_string(),
                source: "/opt/extensions/wordpress".to_string(),
                identity: Some("sha256:content".to_string()),
            }]
        );

        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--isolate-gate-home",
            "false",
            "--isolate-gate-xdg",
            "false",
        ])
        .expect("parse gate isolation opt-outs")
        .gates
        .into();
        assert!(!options.gate_environment.isolate_home);
        assert!(!options.gate_environment.isolate_xdg);
    }

    #[test]
    fn file_gate_snapshots_exact_bytes_and_private_provenance_is_redacted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let public = temp.path().join("public.sh");
        let private = temp.path().join("private.sh");
        let public_program = "for file in src/*.rs; do printf '%s\\n' \"$file\"; done\n";
        let private_program = "printf 'secret $TOKEN'\n";
        fs::write(&public, public_program).expect("write public gate");
        fs::write(&private, private_program).expect("write private gate");
        let mut gates = VerifyGateArgs {
            verify_file: vec![public.display().to_string()],
            private_verify_file: vec![private.display().to_string()],
            ..TestCli::try_parse_from(["homeboy"])
                .expect("parse defaults")
                .gates
        };

        gates.snapshot_file_inputs().expect("snapshot gate files");
        fs::write(&public, "exit 1\n").expect("mutate source after snapshot");

        assert_eq!(gates.verify, vec![public_program]);
        assert_eq!(gates.private_verify, vec![private_program]);
        assert_eq!(gates.input_sources.len(), 2);
        assert_eq!(gates.input_sources[0].source_kind, "file");
        assert_eq!(
            gates.input_sources[0].path.as_deref(),
            Some(public.to_str().unwrap())
        );
        assert!(gates.input_sources[0].sha256.starts_with("sha256:"));
        assert_eq!(gates.input_sources[1].path, None);
        assert_eq!(
            gates.input_sources[1].redaction_policy,
            AgentTaskGateRevealPolicy::SummaryOnly
        );
        let persisted = serde_json::to_value(VerifyGateOptions::from(gates))
            .expect("serialize durable gate policy");
        assert_eq!(persisted["input_sources"][0]["source_kind"], "file");
        assert_eq!(
            persisted["input_sources"][1]["path"],
            serde_json::Value::Null
        );
        assert_eq!(
            persisted["input_sources"][1]["redaction_policy"],
            "summary_only"
        );
        // Private gate command text follows the established trusted durable
        // recipe contract so retry/adoption can replay it; public projections
        // must redact it instead of claiming encrypted storage.
        assert_eq!(persisted["private_verify"][0], private_program);
    }

    #[test]
    fn file_gate_rejects_missing_empty_and_oversized_inputs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let empty = temp.path().join("empty.sh");
        let oversized = temp.path().join("oversized.sh");
        fs::write(&empty, "").expect("write empty gate");
        fs::write(&oversized, vec![b'x'; MAX_GATE_FILE_BYTES as usize + 1])
            .expect("write oversized gate");
        for path in [temp.path().join("missing.sh"), empty, oversized] {
            let error = snapshot_gate_file(
                path.to_str().unwrap(),
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            )
            .expect_err("invalid gate file must fail");
            assert_eq!(
                error.code,
                homeboy::core::ErrorCode::ValidationInvalidArgument
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_gate_rejects_symlinks_and_fifos_without_reading_them() {
        use std::os::unix::fs::symlink;
        use std::process::Command;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.sh");
        let symlink_path = temp.path().join("link.sh");
        let fifo_path = temp.path().join("gate.fifo");
        fs::write(&source, "exit 0\n").expect("write source");
        symlink(&source, &symlink_path).expect("create symlink");
        Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("run mkfifo");
        for path in [&symlink_path, &fifo_path] {
            snapshot_gate_file(
                path.to_str().unwrap(),
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
            )
            .expect_err("unsafe file type must fail");
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_gate_rejects_replaced_path_after_preopen_identity_check() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.sh");
        let path = temp.path().join("gate.sh");
        fs::write(&target, "exit 0\n").expect("write target");
        fs::write(&path, "exit 0\n").expect("write gate");
        // The lstat/open identity comparison is deterministic for a path replaced
        // before open: the symlink is rejected without following it.
        fs::remove_file(&path).expect("remove gate");
        symlink(&target, &path).expect("replace with symlink");
        snapshot_gate_file(
            path.to_str().unwrap(),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
        )
        .expect_err("replacement must fail closed");
    }

    #[test]
    fn gate_toolchain_spec_preserves_non_default_probe_arguments() {
        let options: VerifyGateOptions = TestCli::try_parse_from([
            "homeboy",
            "--gate-toolchain",
            "legacy-tool",
            "--gate-toolchain-spec",
            r#"{"command":"custom-tool","probe_arguments":["probe","--json"]}"#,
        ])
        .expect("parse legacy and structured toolchain declarations")
        .gates
        .into();

        assert_eq!(
            options.gate_toolchains,
            vec![
                AgentTaskGateToolchainRequirement {
                    command: "legacy-tool".to_string(),
                    probe_arguments: vec!["--version".to_string()],
                },
                AgentTaskGateToolchainRequirement {
                    command: "custom-tool".to_string(),
                    probe_arguments: vec!["probe".to_string(), "--json".to_string()],
                },
            ]
        );
    }

    #[derive(Parser)]
    struct CookHelpCli {
        #[command(flatten)]
        cook: AgentTaskCookArgs,
    }

    fn rendered_cook_help() -> String {
        use clap::CommandFactory;
        CookHelpCli::command().render_long_help().to_string()
    }

    fn rendered_cook_batch_help() -> String {
        use clap::CommandFactory;

        crate::cli_surface::Cli::command()
            .find_subcommand("agent-task")
            .expect("agent-task command")
            .find_subcommand("fanout")
            .expect("fanout command")
            .find_subcommand("cook-batch")
            .expect("cook-batch command")
            .clone()
            .render_long_help()
            .to_string()
    }

    #[test]
    fn provider_evidence_help_and_parse_diagnostic_share_the_declaration_contract() {
        let error = parse_provider_evidence_input("/absolute/path")
            .expect_err("bare provider evidence paths must be rejected");
        assert!(error.contains(PROVIDER_EVIDENCE_DECLARATION), "{error}");

        for help in [rendered_cook_help(), rendered_cook_batch_help()] {
            assert!(help.contains(PROVIDER_EVIDENCE_DECLARATION), "{help}");
        }
    }

    #[test]
    fn cook_help_does_not_leak_internal_refactoring_prose() {
        // #9898/#9907: help must describe the operator contract, never the Rust
        // refactor behind the flags.
        let help = rendered_cook_help();
        for leaked in [
            "Flattened into",
            "#[arg] attributes",
            "DispatchArgs",
            "field group is declared once",
            "reproduce the original flag",
            "CLI surface for the dispatch inputs",
        ] {
            assert!(
                !help.contains(leaked),
                "cook help leaked internal prose {leaked:?}:\n{help}"
            );
        }
    }

    #[test]
    fn cook_help_documents_core_workflow_flags() {
        let help = rendered_cook_help();
        // Each core flag renders with operator-facing help, not a blank line.
        assert!(help.contains("--goal"), "{help}");
        assert!(help.contains("Workspace handle the cook edits"), "{help}");
        assert!(
            help.contains("Deterministic verification command"),
            "{help}"
        );
        assert!(help.contains("--verify-file"), "{help}");
        assert!(help.contains("for file in src/*.rs"), "{help}");
        assert!(help.contains("before opening the pull request"), "{help}");
    }

    #[test]
    fn compact_cook_help_explains_backend_resolution() {
        use clap::CommandFactory;

        let command = crate::cli_surface::Cli::command();
        let help = command
            .find_subcommand("agent-task")
            .expect("agent-task command")
            .find_subcommand("cook")
            .expect("Cook command")
            .clone()
            .render_long_help()
            .to_string();
        assert!(
            help.contains("Backend selection: pass --backend explicitly"),
            "{help}"
        );
        assert!(
            help.contains("multiple ready routes require an explicit choice"),
            "{help}"
        );
    }

    #[test]
    fn cook_help_documents_explicit_execution_cap_precedence_over_configured_rotations() {
        let help = rendered_cook_help();
        assert!(
            help.contains("--max-attempts 1 --max-provider-executions 1"),
            "{help}"
        );
        assert!(
            help.contains("explicit `--max-provider-rotations`"),
            "{help}"
        );
    }

    #[test]
    fn cook_parser_preserves_an_explicit_execution_cap_without_a_rotation_override() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--max-attempts",
            "1",
            "--max-provider-executions",
            "1",
            "--no-finalize",
            "--prompt",
            "test",
            "--to-worktree",
            "repo@branch",
        ])
        .expect("parse an explicitly bounded Cook");
        let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("Cook command");
        };

        assert_eq!(cook.max_attempts, 1);
        assert_eq!(cook.dispatch.core.attempts, Some(1));
        assert_eq!(cook.dispatch.core.provider_rotations, None);
    }

    #[test]
    fn cook_help_exposes_quiet_progress_for_orchestration() {
        let help = rendered_cook_help();
        assert!(help.contains("--no-progress"), "{help}");
        assert!(
            help.contains("Suppress intermediate Cook progress"),
            "{help}"
        );
    }

    #[test]
    fn cook_cli_preflight_explains_the_default_provider_budget_conflict() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--max-attempts",
            "3",
            "--no-finalize",
            "--prompt",
            "test",
            "--to-worktree",
            "repo@branch",
        ])
        .expect("parse Cook with the default provider budget");
        let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("Cook command");
        };
        // Resolve through the real budget resolver with no configured rotation:
        // that is the shape this error message exists for.
        let core: homeboy::agents::agent_task_dispatch_service::DispatchCoreInputs =
            cook.dispatch.core.clone().into();
        let budget =
            homeboy::agents::agent_task_dispatch_plan::resolve_execution_budget(&core, None);
        assert_eq!(budget.max_provider_executions, 1);
        assert_eq!(budget.max_provider_rotations, 0);

        let error = homeboy::agents::agent_task_service::validate_effective_cook_budget(
            cook.max_attempts,
            &budget,
        )
        .expect_err("default provider budget must not silently discard Cook retries");
        assert!(
            error
                .message
                .contains("--max-provider-executions 3 --max-same-provider-retries 2"),
            "{}",
            error.message
        );
    }

    #[test]
    fn cook_help_advertises_one_prompt_source_not_wave_inputs() {
        let help = rendered_cook_help();
        assert!(help.contains("--prompt"), "{help}");
        assert!(!help.contains("--task <"), "{help}");
        assert!(!help.contains("--tasks <"), "{help}");
    }

    #[test]
    fn cook_accepts_issue_backed_destination_derivation_and_explicit_override() {
        let derived = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/11225",
            "--no-finalize",
        ])
        .expect("issue-backed Cook parses without an explicit destination");
        let crate::cli_surface::Commands::AgentTask(agent_task) = derived.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(derived) = agent_task.command else {
            panic!("Cook command");
        };
        assert_eq!(derived.to_worktree, None);

        let explicit = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "homeboy@existing",
            "--no-finalize",
        ])
        .expect("explicit Cook destination parses");
        let crate::cli_surface::Commands::AgentTask(agent_task) = explicit.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(explicit) = agent_task.command else {
            panic!("Cook command");
        };
        assert_eq!(explicit.to_worktree.as_deref(), Some("homeboy@existing"));

        let draft = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "homeboy@existing",
            "--draft-pr",
        ])
        .expect("draft Cook parses");
        let crate::cli_surface::Commands::AgentTask(agent_task) = draft.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(draft) = agent_task.command else {
            panic!("Cook command");
        };
        assert!(draft.draft_pr);
        assert!(crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "homeboy@existing",
            "--no-finalize",
            "--draft-pr",
        ])
        .is_err());
    }

    #[test]
    fn self_repair_bootstrap_cannot_disable_finalization() {
        assert!(crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "repair the provider",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/13410",
            "--cwd",
            "/tmp/homeboy-self-repair",
            "--worktree-provider-self-repair",
            "fixture",
            "--no-finalize",
        ])
        .is_err());
    }
}

#[derive(Args, Debug, Clone)]
#[command(disable_help_flag = true)]
pub struct AgentTaskCookArgs {
    /// Show compact task-first Cook help. Use `--help-full` for the complete
    /// Cook option reference.
    #[arg(short = 'h', long, action = clap::ArgAction::HelpShort)]
    pub help: Option<bool>,
    /// Show the complete Cook option reference.
    #[arg(long = "help-full", action = clap::ArgAction::HelpLong)]
    pub help_full: Option<bool>,
    #[command(flatten)]
    pub dispatch: DispatchArgs,
    /// Completion rule for isolated candidates: wait for all results (default)
    /// or promote the first successful candidate.
    #[arg(long, default_value_t = AgentTaskCandidateCompletionPolicy::WaitAll, value_name = "POLICY")]
    pub candidate_completion: AgentTaskCandidateCompletionPolicy,
    #[arg(long, hide = true)]
    pub attempt_run_id: Option<String>,
    #[arg(long, hide = true)]
    pub attempt_plan: Option<String>,
    /// Resolve the Cook plan and validate static inputs without creating a run
    /// or provisioning a worktree. Includes a replayable command.
    #[arg(long)]
    pub preview: bool,
    /// One-line statement of what a successful cook must achieve. Recorded as
    /// framing metadata for the provider task and used for review. Without
    /// --prompt, it supplies the one provider task.
    #[arg(long, value_name = "TEXT")]
    pub goal: Option<String>,
    #[arg(long = "provider-evidence", value_name = "JSON", help = PROVIDER_EVIDENCE_DECLARATION, value_parser = parse_provider_evidence_input)]
    pub provider_evidence_inputs: Vec<AgentTaskProviderEvidenceInput>,
    /// Workspace handle the cook edits, verifies, and finalizes into. The handle
    /// is `<repo>@<branch-slug>`, where the slug replaces every character of
    /// --head outside [A-Za-z0-9_-] with `-`, so branch `fix/1234-x` is handle
    /// `repo@fix-1234-x`. Existing destinations are reused. Creating a missing
    /// one is not a built-in capability: it requires an enabled worktree
    /// provider with a `commands.ensure` argv template, and without one you must
    /// create the destination first with `homeboy worktree create`. When
    /// omitted, an explicit --cwd is the canonical destination. Otherwise,
    /// --repo plus --task-url derives an issue-owned destination through that
    /// same configured provider. An explicit --workspace or --cwd Git checkout
    /// can infer --repo when its remote maps to exactly one
    /// configured component; an explicit --repo must match that checkout. When
    /// paired with --cwd, this must name the same existing local or active
    /// registered linked task worktree; --cwd remains the Cook workspace
    /// authority.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: Option<String>,
    /// Temporarily use the explicit clean --cwd as workspace authority while
    /// repairing the configured provider that owns this repository. The
    /// provider must declare its repository under
    /// settings.worktree_provider_self_repair; normal Cook gates, review, PR
    /// finalization, and durable provenance remain active.
    #[arg(
        long,
        value_name = "PROVIDER_ID",
        requires = "cwd",
        conflicts_with_all = ["workspace", "to_worktree", "no_finalize"]
    )]
    pub worktree_provider_self_repair: Option<String>,
    #[arg(
        long,
        value_name = "COMMAND",
        long_help = "Deprecated promotion apply-provider command string. Migrate `--provider-command 'provider --flag value'` to `--provider-argv provider --provider-argv --flag --provider-argv value`; argv preserves exact arguments without shell splitting. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with `workspace_path`."
    )]
    pub provider_command: Option<String>,
    #[arg(
        long = "provider-argv",
        value_name = "ARG",
        conflicts_with = "provider_command",
        long_help = "Promotion-only apply-provider invocation argument. Repeat once per exact argv element: the first is the executable and later values are its arguments; values are never shell-split. This cannot select an executor. The provider reads stdin request schema `homeboy/agent-task-promotion-apply-request/v1` and writes response schema `homeboy/agent-task-promotion-apply-response/v1` with required `workspace_path`."
    )]
    pub provider_argv: Vec<String>,
    #[command(flatten)]
    pub gates: VerifyGateArgs,
    /// Maximum Cook attempts before giving up. Each attempt re-runs the agent
    /// and gates; a later attempt can recover from a transient failure. This
    /// derives provider execution and same-provider remediation budgets. A
    /// configured provider rotation receives its own additional execution
    /// allowance unless an advanced budget flag explicitly caps it (default 3).
    #[arg(
        long = "max-attempts",
        default_value_t = 3,
        value_parser = clap::value_parser!(u32).range(1..),
        value_name = "N"
    )]
    pub max_attempts: u32,
    /// Stop after the work is verified but before opening the pull request,
    /// leaving the committed change on the worktree branch for manual review or
    /// a later `agent-task review`/finalize.
    #[arg(long = "no-finalize")]
    pub no_finalize: bool,
    /// Complete normal verified finalization but create a draft pull request.
    /// Existing pull requests retain their current draft or ready state.
    #[arg(long = "draft-pr", conflicts_with = "no_finalize")]
    pub draft_pr: bool,
    /// Return the complete cook report, including nested promotion and gate evidence.
    #[arg(long)]
    pub full: bool,
    /// Suppress intermediate Cook progress lines after the durable run identity.
    /// The final result still contains status and evidence commands for orchestration.
    #[arg(long)]
    pub no_progress: bool,
    /// Base branch the finalized pull request targets and the branch changes are
    /// diffed against. When omitted, Cook resolves configured repository or
    /// remote default-branch evidence before retaining its deferred `main`
    /// compatibility default when the provider has not materialized a checkout.
    #[arg(long, value_name = "BRANCH")]
    pub base: Option<String>,
    /// Head branch to push and open the PR from. Defaults to the branch the
    /// destination worktree is already on.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,
    /// Title for the finalized pull request. Defaults to a title derived from
    /// the goal / commit.
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Commit message for the cook's committed change. Defaults to a message
    /// derived from the goal.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: Option<String>,
    /// Branch names the cook refuses to push to or target directly, as a safety
    /// guard. Repeatable; defaults to the standard protected set.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,
    /// AI tool disclosure recorded in the PR's assistance attribution
    /// (default `AI-assisted`).
    #[arg(long, default_value = "AI-assisted", value_name = "TEXT")]
    pub ai_tool: String,
    /// Legacy AI-usage disclosure. The reviewer-facing "Used for" text is now
    /// authored by the agent's `review_form.used_for` (a self-reflective process
    /// description) and validated by the cook loop's review-form gate; this flag
    /// no longer feeds the PR body. Retained only for recipe back-compatibility
    /// and defaults empty (no canned platitude).
    #[arg(long, default_value = "", value_name = "TEXT")]
    pub ai_used_for: String,
    /// Require a separate durable acceptance verdict before PR finalization.
    #[arg(long)]
    pub require_acceptance: bool,
    /// Authority allowed to issue the acceptance verdict.
    #[arg(long, requires = "require_acceptance")]
    pub acceptance_authority: Option<String>,
    /// Policy the acceptance authority applies.
    #[arg(long, requires = "require_acceptance")]
    pub acceptance_policy: Option<String>,
    /// Controller-resolved repository identity for a supplied checkout. This is
    /// not caller input: Cook persists it with the compiled plan.
    #[arg(skip)]
    pub repository_identity: Option<serde_json::Value>,
    /// Controller-resolved base provenance persisted with the Cook plan.
    #[arg(skip)]
    pub base_resolution: Option<serde_json::Value>,
    /// Captured at CLI ingress when `--prompt -` is used. This survives route
    /// handoff and plan compilation without asking a later phase to reread stdin.
    #[arg(skip)]
    pub prompt_snapshot: Option<CookPromptSnapshot>,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct CookPromptSnapshot {
    #[serde(skip)]
    pub content: String,
    pub source: String,
    pub sha256: String,
    pub size_bytes: usize,
}

pub(crate) fn parse_provider_evidence_input(
    value: &str,
) -> Result<AgentTaskProviderEvidenceInput, String> {
    serde_json::from_str(value).map_err(|error| {
        format!("invalid provider evidence declaration: {error}. {PROVIDER_EVIDENCE_DECLARATION}")
    })
}

#[derive(Args, Clone, Debug)]
pub struct PromotionProviderArgs {
    #[arg(long, value_name = "PATH")]
    pub workspace: String,
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopArgs {
    #[command(subcommand)]
    pub command: AgentTaskLoopCommand,
}
#[derive(Subcommand, Debug)]
pub enum AgentTaskLoopCommand {
    /// Define or update a durable loop from a spec.
    ///
    /// `--on`/`--off` set whether the loop runs; `--revolution-limit` bounds how
    /// many revolutions it may take before it stops on its own.
    Define(AgentTaskLoopDefineArgs),
    /// Read durable loop state: on/off, revolutions taken, and continuation policy.
    Status(AgentTaskLoopStatusArgs),
    /// Resume a stopped or exhausted durable loop, optionally raising its
    /// revolution limit.
    Resume(AgentTaskLoopResumeArgs),
    /// Stop a durable loop and record the handoff.
    Stop(AgentTaskLoopStatusArgs),
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopDefineArgs {
    #[arg(value_name = "SPEC")]
    pub spec: String,
    #[arg(long, conflicts_with = "off")]
    pub on: bool,
    #[arg(long, conflicts_with = "on")]
    pub off: bool,
    #[arg(long = "revolution-limit", value_name = "N")]
    pub revolution_limit: Option<u32>,
    #[arg(long)]
    pub resume: bool,
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopStatusArgs {
    pub loop_id: String,
}
#[derive(Args, Debug)]
pub struct AgentTaskLoopResumeArgs {
    pub loop_id: String,
    #[arg(long = "revolution-limit", value_name = "N")]
    pub revolution_limit: Option<u32>,
    #[arg(long = "dispatch-backend", value_name = "BACKEND")]
    pub dispatch_backend: Option<String>,
    #[arg(
        long = "dispatch-selector",
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub dispatch_selector: Option<String>,
    #[arg(long = "dispatch-model", value_name = "MODEL")]
    pub dispatch_model: Option<String>,
    #[arg(long = "dispatch-provider-config", value_name = "JSON")]
    pub dispatch_provider_config: Option<String>,
}
