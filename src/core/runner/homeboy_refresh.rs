use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::error::{Error, Result};
use crate::core::output::MergeOutput;

use super::{connect, disconnect, exec, load, merge, RunnerCapabilityPreflight, RunnerExecOptions};

const DEFAULT_HOMEBOY_REMOTE: &str = "https://github.com/Extra-Chill/homeboy.git";
const DEFAULT_HOMEBOY_REF: &str = "main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeboyBinaryRefreshMode {
    Materialize,
    Select { binary_path: String },
}

#[derive(Debug, Clone)]
pub struct HomeboyBinaryRefreshOptions {
    pub runner_id: String,
    pub mode: HomeboyBinaryRefreshMode,
    pub source: Option<String>,
    pub git_ref: Option<String>,
    pub target_dir: Option<String>,
    pub reconnect: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyBinaryRefreshPlan {
    pub runner_id: String,
    pub mode: String,
    pub source: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    pub target_dir: Option<String>,
    pub binary_path: String,
    pub script: String,
    pub reconnect: bool,
    pub followup_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyBinaryRefreshOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub plan: HomeboyBinaryRefreshPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Value>,
    pub updated_fields: Vec<String>,
    pub daemon_refreshed: bool,
    pub followup_commands: Vec<String>,
}

pub fn plan_homeboy_binary_refresh(
    options: &HomeboyBinaryRefreshOptions,
) -> Result<HomeboyBinaryRefreshPlan> {
    let runner = load(&options.runner_id)?;
    let runner_id = runner.id;
    match &options.mode {
        HomeboyBinaryRefreshMode::Select { binary_path } => {
            let binary_path = non_empty("select", binary_path)?;
            let script = identity_probe_script(binary_path);
            Ok(HomeboyBinaryRefreshPlan {
                runner_id: runner_id.clone(),
                mode: "select".to_string(),
                source: None,
                git_ref: None,
                target_dir: None,
                binary_path: binary_path.to_string(),
                script,
                reconnect: options.reconnect,
                followup_commands: refresh_followups(&runner_id, options.reconnect),
            })
        }
        HomeboyBinaryRefreshMode::Materialize => {
            let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "target_dir",
                    "runner refresh-homeboy requires --target-dir when the runner has no workspace_root",
                    None,
                    None,
                )
            })?;
            let source = options
                .source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_HOMEBOY_REMOTE);
            let git_ref = options
                .git_ref
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_HOMEBOY_REF);
            let target_dir = options
                .target_dir
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| default_target_dir(workspace_root, git_ref));
            let binary_path = format!(
                "{}/target/release/homeboy",
                target_dir.trim_end_matches('/')
            );
            let script = materialize_script(source, git_ref, &target_dir, &binary_path);
            Ok(HomeboyBinaryRefreshPlan {
                runner_id: runner_id.clone(),
                mode: "materialize".to_string(),
                source: Some(source.to_string()),
                git_ref: Some(git_ref.to_string()),
                target_dir: Some(target_dir),
                binary_path,
                script,
                reconnect: options.reconnect,
                followup_commands: refresh_followups(&runner_id, options.reconnect),
            })
        }
    }
}

pub fn refresh_homeboy_binary(
    options: HomeboyBinaryRefreshOptions,
) -> Result<(HomeboyBinaryRefreshOutput, i32)> {
    let plan = plan_homeboy_binary_refresh(&options)?;
    if options.dry_run {
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: true,
                identity: None,
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                followup_commands: plan.followup_commands.clone(),
                plan,
            },
            0,
        ));
    }

    let required_commands = match &options.mode {
        HomeboyBinaryRefreshMode::Materialize => {
            vec!["bash".to_string(), "git".to_string(), "cargo".to_string()]
        }
        HomeboyBinaryRefreshMode::Select { .. } => vec!["bash".to_string()],
    };

    let (exec_output, exit_code) = exec(
        &plan.runner_id,
        RunnerExecOptions {
            cwd: None,
            project_id: None,
            allow_diagnostic_ssh: true,
            command: vec!["bash".to_string(), "-lc".to_string(), plan.script.clone()],
            env: Default::default(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: Some(RunnerCapabilityPreflight {
                command: "runner.refresh-homeboy".to_string(),
                required_commands,
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
        },
    )?;
    if exit_code != 0 {
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: false,
                identity: None,
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                followup_commands: plan.followup_commands.clone(),
                plan,
            },
            exit_code,
        ));
    }

    let identity = parse_identity(&exec_output.stdout)?;
    let patch = serde_json::json!({ "homeboy_path": plan.binary_path });
    let updated_fields = match merge(Some(&plan.runner_id), &patch.to_string(), &[])? {
        MergeOutput::Single(result) => result.updated_fields,
        MergeOutput::Bulk(_) => Vec::new(),
    };

    let mut daemon_refreshed = false;
    if options.reconnect {
        let _ = disconnect(&plan.runner_id);
        let (_report, connect_exit_code) = connect(&plan.runner_id)?;
        daemon_refreshed = connect_exit_code == 0;
    }

    Ok((
        HomeboyBinaryRefreshOutput {
            variant: "refresh_homeboy",
            command: "runner.refresh_homeboy",
            runner_id: plan.runner_id.clone(),
            dry_run: false,
            plan: plan.clone(),
            identity: Some(identity),
            updated_fields,
            daemon_refreshed,
            followup_commands: plan.followup_commands,
        },
        0,
    ))
}

fn materialize_script(source: &str, git_ref: &str, target_dir: &str, binary_path: &str) -> String {
    format!(
        "set -e\nsource={}\nref={}\ndir={}\nbinary={}\nmkdir -p \"$(dirname \"$dir\")\"\nif [ ! -d \"$dir/.git\" ]; then\n  git clone \"$source\" \"$dir\"\nfi\ncurrent_remote=$(git -C \"$dir\" config --get remote.origin.url 2>/dev/null || true)\nif [ \"$current_remote\" != \"$source\" ]; then\n  git -C \"$dir\" remote set-url origin \"$source\" 2>/dev/null || git -C \"$dir\" remote add origin \"$source\"\nfi\ngit -C \"$dir\" fetch --prune origin\ntarget=$(git -C \"$dir\" rev-parse --verify --quiet \"origin/$ref\" || git -C \"$dir\" rev-parse --verify --quiet \"$ref\")\nif [ -z \"$target\" ]; then\n  echo \"Homeboy ref not found: $ref\" >&2\n  exit 1\nfi\ngit -C \"$dir\" checkout --quiet --force --detach \"$target\"\ngit -C \"$dir\" reset --hard \"$target\"\ncargo build --release --bin homeboy --manifest-path \"$dir/Cargo.toml\"\n\"$binary\" self identity\n",
        sq(source),
        sq(git_ref),
        sq(target_dir),
        sq(binary_path),
    )
}

fn identity_probe_script(binary_path: &str) -> String {
    format!(
        "set -e\nbinary={}\n\"$binary\" self identity\n",
        sq(binary_path)
    )
}

fn parse_identity(stdout: &str) -> Result<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(Error::internal_json(
            "refresh-homeboy produced no identity output".to_string(),
            None,
        ));
    }
    serde_json::from_str(trimmed).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner Homeboy identity output".to_string()),
        )
    })
}

fn default_target_dir(workspace_root: &str, git_ref: &str) -> String {
    format!(
        "{}/_homeboy_binaries/homeboy-{}",
        workspace_root.trim_end_matches('/'),
        sanitize_ref(git_ref)
    )
}

fn refresh_followups(runner_id: &str, reconnect: bool) -> Vec<String> {
    if reconnect {
        vec![format!("homeboy runner status {}", shell_arg(runner_id))]
    } else {
        vec![
            format!("homeboy runner disconnect {}", shell_arg(runner_id)),
            format!("homeboy runner connect {}", shell_arg(runner_id)),
            format!("homeboy runner status {}", shell_arg(runner_id)),
        ]
    }
}

fn non_empty<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            name,
            format!("{name} must not be empty"),
            None,
            None,
        ));
    }
    Ok(trimmed)
}

fn sanitize_ref(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    sanitized.trim_matches('-').to_string().if_empty("main")
}

trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn sq(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn shell_arg(value: &str) -> String {
    sq(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_plan_uses_clean_runner_cache() {
        let options = HomeboyBinaryRefreshOptions {
            runner_id: "lab".to_string(),
            mode: HomeboyBinaryRefreshMode::Materialize,
            source: Some("https://example.test/homeboy.git".to_string()),
            git_ref: Some("fix/foo".to_string()),
            target_dir: Some("/runner/ws/homeboy-clean".to_string()),
            reconnect: false,
            dry_run: true,
        };
        let plan = HomeboyBinaryRefreshPlan {
            runner_id: "lab".to_string(),
            mode: "materialize".to_string(),
            source: options.source.clone(),
            git_ref: options.git_ref.clone(),
            target_dir: options.target_dir.clone(),
            binary_path: "/runner/ws/homeboy-clean/target/release/homeboy".to_string(),
            script: materialize_script(
                "https://example.test/homeboy.git",
                "fix/foo",
                "/runner/ws/homeboy-clean",
                "/runner/ws/homeboy-clean/target/release/homeboy",
            ),
            reconnect: false,
            followup_commands: refresh_followups("lab", false),
        };

        assert!(plan.script.contains("git clone \"$source\" \"$dir\""));
        assert!(plan.script.contains("checkout --quiet --force --detach"));
        assert!(plan.script.contains("cargo build --release --bin homeboy"));
        assert_eq!(
            plan.binary_path,
            "/runner/ws/homeboy-clean/target/release/homeboy"
        );
    }

    #[test]
    fn select_plan_only_probes_requested_binary() {
        let script = identity_probe_script("/opt/homeboy/bin/homeboy");

        assert!(script.contains("binary='/opt/homeboy/bin/homeboy'"));
        assert!(script.contains("\"$binary\" self identity"));
        assert!(!script.contains("cargo build"));
    }

    #[test]
    fn default_target_dir_is_ref_scoped() {
        assert_eq!(
            default_target_dir("/runner/ws/", "origin/main"),
            "/runner/ws/_homeboy_binaries/homeboy-origin-main"
        );
    }
}
