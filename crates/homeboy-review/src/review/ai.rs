//! Independent advisory reviews produced by operator-installed extensions.
//! Reuses manifest contract producers and the shared bounded process owner.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use homeboy_core::engine::execution_context::ExecutionContext;
use homeboy_core::extension::catalog::load_extension;
use homeboy_core::extension::invoke::deadline_process::execute_deadline_process;
use homeboy_core::{Error, Result};
use homeboy_extension_contract::extension_contract_producer::{
    ExtensionContractProducerPhase, EXTENSION_CONTRACT_PRODUCER_SCHEMA,
};
use homeboy_finding::HomeboyFinding;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{ReviewArtifactCommand, ReviewCommandOutput};

pub const REQUEST_SCHEMA: &str = "homeboy/ai-review-request/v1";
pub const RESPONSE_SCHEMA: &str = "homeboy/ai-review-response/v1";
pub const CAPTURE_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiReviewExecution {
    Completed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AiReviewVerdict {
    Pass,
    Findings,
    Block,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiReviewRequest {
    pub schema: String,
    pub component: String,
    pub checkout: String,
    pub base_sha: String,
    pub head_sha: String,
    pub settings: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiReviewResponse {
    pub schema: String,
    pub execution: AiReviewExecution,
    pub verdict: AiReviewVerdict,
    pub base_sha: String,
    pub head_sha: String,
    #[serde(default)]
    pub findings: Vec<HomeboyFinding>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub provenance: Value,
    #[serde(default)]
    pub usage: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiReviewResult {
    pub request: AiReviewRequest,
    pub provider: String,
    pub execution: AiReviewExecution,
    pub verdict: AiReviewVerdict,
    pub findings: Vec<HomeboyFinding>,
    pub reason: Option<String>,
    pub provenance: Value,
    pub usage: Value,
    pub raw_stdout: String,
    pub raw_stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

impl AiReviewResult {
    /// Model concerns are advisory. Skips and execution failures are explicitly
    /// incomplete, never a successful review of this candidate.
    pub fn command_exit(&self) -> i32 {
        if self.execution == AiReviewExecution::Completed {
            0
        } else {
            2
        }
    }

    pub fn artifact_command(&self) -> ReviewArtifactCommand {
        ReviewArtifactCommand {
            name: "ai".to_string(),
            status: match self.execution {
                AiReviewExecution::Completed => "passed",
                AiReviewExecution::Skipped => "skipped",
                AiReviewExecution::Failed => "failed",
            }
            .to_string(),
            exit_code: self.command_exit(),
            summary: format!(
                "{:?}: {:?}; {} finding(s){}",
                self.execution,
                self.verdict,
                self.findings.len(),
                self.reason
                    .as_ref()
                    .map(|s| format!("; {s}"))
                    .unwrap_or_default()
            ),
            findings: self.findings.clone(),
            artifacts: vec![json!({"kind": "ai-review", "result": self})],
        }
    }

    pub fn attach(self, output: &mut ReviewCommandOutput) -> i32 {
        let code = self.command_exit();
        output.summary.total_findings += self.findings.len();
        output.artifact.commands.push(self.artifact_command());
        output.artifact.base_ref = self.request.base_sha.clone();
        output.artifact.head_ref = self.request.head_sha.clone();
        if code != 0 {
            output.summary.passed = false;
            output.summary.status = "incomplete".to_string();
            output.summary.hints.push(
                self.reason
                    .clone()
                    .unwrap_or_else(|| "AI review did not complete".to_string()),
            );
        }
        output.artifact.status = super::artifact_status(&output.artifact.commands).to_string();
        output.ai = Some(self);
        code
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::validation_invalid_argument("ai-review", message, None, None)
}

fn git(checkout: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .output()
        .map_err(|e| invalid(format!("git execution failed: {e}")))?;
    if !output.status.success() {
        return Err(invalid("cannot resolve committed review scope"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Bind surrounding-code reads to the same committed candidate as the diff.
/// Working-tree changes are deliberately unsupported by this v1 contract.
pub fn candidate(checkout: &Path, base: &str) -> Result<(String, String)> {
    if !git(
        checkout,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        return Err(invalid(
            "AI review requires a clean committed checkout; working-tree review is unsupported",
        ));
    }
    let base = git(
        checkout,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?;
    let head = git(checkout, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let base = git(checkout, &["merge-base", &base, &head])?;
    Ok((base, head))
}

/// Discover exactly one linked producer declaring the shared request/response.
/// A private local extension is as valid as a published extension.
pub fn run(context: &ExecutionContext, base: &str, timeout: Duration) -> Result<AiReviewResult> {
    if timeout.is_zero() || timeout > Duration::from_secs(3600) {
        return Err(invalid("review timeout must be between 1 and 3600 seconds"));
    }
    let (base_sha, head_sha) = candidate(&context.source_path, base)?;
    let mut providers = Vec::new();
    for (id, scoped) in context.component.extensions.as_ref().into_iter().flatten() {
        let manifest = load_extension(id)?;
        for producer in &manifest.contract_producers {
            if producer.invocation.output_schema.as_deref() != Some(RESPONSE_SCHEMA) {
                continue;
            }
            if producer.schema != EXTENSION_CONTRACT_PRODUCER_SCHEMA
                || producer.phase != ExtensionContractProducerPhase::Result
                || producer.invocation.input_schema.as_deref() != Some(REQUEST_SCHEMA)
            {
                return Err(invalid(
                    "review producer has an invalid phase or schema declaration",
                ));
            }
            let mut settings = BTreeMap::new();
            for setting in &manifest.settings {
                if let Some(value) = &setting.default {
                    settings.insert(setting.id.clone(), value.clone());
                }
            }
            settings.extend(scoped.settings.iter().map(|(k, v)| (k.clone(), v.clone())));
            settings.extend(context.settings.iter().cloned());
            providers.push((
                id.clone(),
                manifest.extension_path.clone(),
                producer.clone(),
                settings,
            ));
        }
    }
    if providers.len() != 1 {
        return Err(invalid(format!("expected one linked AI review producer, found {}; install/link a reviewer extension or select it with --extension", providers.len())));
    }
    let (extension_id, root, producer, settings) = providers.remove(0);
    let root = std::fs::canonicalize(
        root.ok_or_else(|| invalid("review extension has no installation path"))?,
    )
    .map_err(|e| invalid(format!("review extension path is unavailable: {e}")))?;
    let script = std::fs::canonicalize(root.join(&producer.invocation.script))
        .map_err(|e| invalid(format!("review producer script is unavailable: {e}")))?;
    if !script.starts_with(&root) || !script.is_file() {
        return Err(invalid(
            "review producer script must be a regular file inside its installed extension",
        ));
    }
    let request = AiReviewRequest {
        schema: REQUEST_SCHEMA.to_string(),
        component: context.component_id.clone(),
        checkout: context.source_path.to_string_lossy().into_owned(),
        base_sha,
        head_sha,
        settings,
    };
    let mut command = Command::new(script);
    command
        .args(&producer.invocation.args)
        .current_dir(&root)
        .env_clear();
    // Only explicitly declared environment reaches the provider; credentials
    // belong to the installed adapter, never the public request or CLI flags.
    let mut secrets = Vec::new();
    for name in &producer.invocation.env {
        if let Ok(value) = std::env::var(name) {
            command.env(name, &value);
            if value.len() >= 8 && name != "PATH" && name != "HOME" {
                secrets.push(value);
            }
        }
    }
    let started = Instant::now();
    let input = serde_json::to_vec(&request).map_err(|e| invalid(e.to_string()))?;
    if input.len() > CAPTURE_LIMIT {
        return Err(invalid("review request exceeds capture limit"));
    }
    let mut result = AiReviewResult {
        request,
        provider: format!("{extension_id}/{}", producer.id),
        execution: AiReviewExecution::Failed,
        verdict: AiReviewVerdict::Inconclusive,
        findings: Vec::new(),
        reason: None,
        provenance: Value::Null,
        usage: Value::Null,
        raw_stdout: String::new(),
        raw_stderr: String::new(),
        exit_code: -1,
        duration_ms: 0,
    };
    match execute_deadline_process(
        command,
        &input,
        started + timeout,
        Duration::from_secs(2),
        CAPTURE_LIMIT,
        "AI review",
    ) {
        Err(e) => result.reason = Some(e.message),
        Ok(output) => {
            result.exit_code = output.status.code().unwrap_or(-1);
            result.raw_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            result.raw_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            match serde_json::from_slice::<AiReviewResponse>(&output.stdout) {
                Ok(response)
                    if response.schema == RESPONSE_SCHEMA
                        && response.base_sha == result.request.base_sha
                        && response.head_sha == result.request.head_sha =>
                {
                    result.execution = response.execution;
                    result.verdict = response.verdict;
                    result.findings = response.findings;
                    result.reason = response.reason;
                    result.provenance = response.provenance;
                    result.usage = response.usage;
                    if !output.status.success() {
                        result.execution = AiReviewExecution::Failed;
                        result.reason =
                            Some(format!("review producer exited {}", result.exit_code));
                    }
                }
                _ => {
                    result.reason =
                        Some("invalid review response schema or candidate identity".to_string())
                }
            }
        }
    }
    if candidate(&context.source_path, base).ok()
        != Some((
            result.request.base_sha.clone(),
            result.request.head_sha.clone(),
        ))
    {
        result.execution = AiReviewExecution::Failed;
        result.reason = Some("candidate changed during review; evidence is stale".to_string());
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    // Redact explicit environment values from every returned field, including
    // provider-authored findings and raw streams, before artifact retention.
    let mut wire = serde_json::to_string(&result).map_err(|e| invalid(e.to_string()))?;
    for secret in secrets {
        let escaped = serde_json::to_string(&secret).map_err(|e| invalid(e.to_string()))?;
        wire = wire.replace(&escaped[1..escaped.len() - 1], "[REDACTED]");
    }
    serde_json::from_str(&wire).map_err(|e| invalid(e.to_string()))
}
