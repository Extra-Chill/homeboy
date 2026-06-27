use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::agent_task::{
    AgentTaskDiagnostic, AgentToolExecutionLocation, AgentToolPolicy, AgentToolRequest,
    AgentToolResult, AgentToolResultStatus, AGENT_TOOL_RESULT_SCHEMA,
};
use crate::core::stream_capture::StreamCaptureMetadata;
use crate::core::{git, worktree};

pub const AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA: &str = "homeboy/agent-tool-dispatch-evidence/v1";

/// Maximum number of bytes retained per captured command stream when surfacing a
/// control-plane tool failure. The dispatched commands' stdout/stderr are
/// agent-influenced and otherwise unbounded, so the retained evidence is capped
/// with truncation metadata before it lands in a diagnostic payload. Mirrors the
/// bounded-capture pattern used by `agent_task_promotion` / runner exec captures
/// (#5363).
const COMMAND_CAPTURE_LIMIT_BYTES: usize = 65_536;

mod capture {
    use super::*;

    /// Bound a captured stream to a retained-byte cap, keeping the trailing bytes
    /// (the most relevant tail for a failure message) and returning the retained
    /// text plus truncation metadata. Mirrors the `bound_captured_stream` pattern in
    /// `agent_task_promotion` so a pathological command cannot force an arbitrarily
    /// large failure payload into memory or logs.
    pub(crate) fn bound_captured_stream(
        bytes: &[u8],
        limit: usize,
    ) -> (String, StreamCaptureMetadata) {
        let seen = bytes.len();
        let retained: &[u8] = if seen > limit {
            &bytes[seen - limit..]
        } else {
            bytes
        };
        let metadata = StreamCaptureMetadata {
            limit_bytes: limit,
            seen_bytes: seen,
            retained_bytes: retained.len(),
            truncated: seen > retained.len(),
        };
        (String::from_utf8_lossy(retained).to_string(), metadata)
    }
}

mod dispatch {
    use super::*;

    pub trait AgentToolControlPlaneDispatcher {
        fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult;
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct UnsupportedAgentToolControlPlaneDispatcher;

    impl AgentToolControlPlaneDispatcher for UnsupportedAgentToolControlPlaneDispatcher {
        fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult {
            unsupported_control_plane_result(request)
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct HomeboyAgentToolControlPlaneDispatcher;

    impl AgentToolControlPlaneDispatcher for HomeboyAgentToolControlPlaneDispatcher {
        fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult {
            match dispatch_homeboy_control_plane_tool(request) {
                Ok(output) => succeeded_tool_result(request, output),
                Err(diagnostic) => failed_tool_result(request, diagnostic),
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AgentToolDispatchOutcome {
        pub location: AgentToolExecutionLocation,
        pub result: AgentToolResult,
        pub evidence: AgentToolDispatchEvidence,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct AgentToolDispatchEvidence {
        pub schema: String,
        pub location: AgentToolExecutionLocation,
        pub request: AgentToolRequest,
        pub result: AgentToolResult,
    }

    pub fn dispatch_agent_tool_request(
        policy: &AgentToolPolicy,
        request: &AgentToolRequest,
        dispatcher: &impl AgentToolControlPlaneDispatcher,
    ) -> AgentToolDispatchOutcome {
        let location = policy.execution_location_for(&request.tool);
        let result = match location {
            AgentToolExecutionLocation::Disabled => disabled_tool_result(request),
            AgentToolExecutionLocation::ControlPlane => dispatcher.dispatch(request),
            AgentToolExecutionLocation::Runner => runner_owned_tool_result(request),
        };
        let evidence = AgentToolDispatchEvidence {
            schema: AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA.to_string(),
            location,
            request: request.redacted(),
            result: result.redacted(),
        };

        AgentToolDispatchOutcome {
            location,
            result,
            evidence,
        }
    }
}

mod results {
    use super::*;

    pub(crate) fn disabled_tool_result(request: &AgentToolRequest) -> AgentToolResult {
        AgentToolResult {
            schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            tool: request.tool.clone(),
            status: AgentToolResultStatus::Denied,
            output: Value::Null,
            diagnostics: vec![AgentTaskDiagnostic {
                class: "agent_tool.disabled".to_string(),
                message: format!("tool '{}' is disabled by agent tool policy", request.tool),
                data: json!({ "tool": request.tool }),
            }],
            metadata: json!({ "execution_location": "disabled" }),
        }
    }

    pub(crate) fn runner_owned_tool_result(request: &AgentToolRequest) -> AgentToolResult {
        AgentToolResult {
            schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            tool: request.tool.clone(),
            status: AgentToolResultStatus::Failed,
            output: Value::Null,
            diagnostics: vec![AgentTaskDiagnostic {
                class: "agent_tool.runner_dispatch_not_handled".to_string(),
                message: "runner tool execution is owned by the provider runtime, not the control-plane dispatcher".to_string(),
                data: json!({ "tool": request.tool }),
            }],
            metadata: json!({ "execution_location": "runner" }),
        }
    }

    pub(crate) fn unsupported_control_plane_result(request: &AgentToolRequest) -> AgentToolResult {
        AgentToolResult {
            schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            tool: request.tool.clone(),
            status: AgentToolResultStatus::Failed,
            output: Value::Null,
            diagnostics: vec![AgentTaskDiagnostic {
                class: "agent_tool.control_plane_dispatch_unsupported".to_string(),
                message: "control-plane tool dispatch is selected by policy, but no dispatcher is registered for this provider execution".to_string(),
                data: json!({ "tool": request.tool }),
            }],
            metadata: json!({ "execution_location": "control_plane" }),
        }
    }

    pub(crate) fn succeeded_tool_result(
        request: &AgentToolRequest,
        output: Value,
    ) -> AgentToolResult {
        AgentToolResult {
            schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            tool: request.tool.clone(),
            status: AgentToolResultStatus::Succeeded,
            output,
            diagnostics: Vec::new(),
            metadata: json!({ "execution_location": "control_plane" }),
        }
    }

    pub(crate) fn failed_tool_result(
        request: &AgentToolRequest,
        diagnostic: AgentTaskDiagnostic,
    ) -> AgentToolResult {
        AgentToolResult {
            schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            tool: request.tool.clone(),
            status: AgentToolResultStatus::Failed,
            output: Value::Null,
            diagnostics: vec![diagnostic],
            metadata: json!({ "execution_location": "control_plane" }),
        }
    }
}

mod tools {
    use super::*;

    pub(crate) fn dispatch_homeboy_control_plane_tool(
        request: &AgentToolRequest,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let input = request.input.as_object().ok_or_else(|| {
            validation_error(
                "input",
                "tool input must be a JSON object",
                json!({ "tool": request.tool }),
            )
        })?;

        match request.tool.as_str() {
            "workspace_read" => workspace_read(input),
            "workspace_grep" => workspace_grep(input),
            "workspace_write" => workspace_write(input),
            "workspace_edit" => workspace_edit(input),
            "workspace_apply_patch" | "apply_patch" => workspace_apply_patch(input),
            "workspace_git_status" => workspace_git_status(input),
            "workspace_git_diff" => workspace_git_diff(input),
            "workspace_git_add" => workspace_git_add(input),
            "workspace_git_commit" => workspace_git_commit(input),
            "workspace_git_push" => workspace_git_push(input),
            "workspace_worktree_add" => workspace_worktree_add(input),
            "get_github_issue" => github_issue_get(input),
            "create_github_issue" => github_issue_create(input),
            "list_github_pulls" => github_pulls_list(input),
            "create_github_pull_request" => github_pull_create(input),
            "comment_github_pull_request" => github_pull_comment(input),
            _ => Err(AgentTaskDiagnostic {
                class: "agent_tool.control_plane_tool_unknown".to_string(),
                message: format!("unsupported control-plane tool '{}'", request.tool),
                data: json!({ "tool": request.tool }),
            }),
        }
    }

    pub(crate) fn workspace_read(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root(input)?;
        let file = workspace_file_path(&root, required_string(input, &["path", "file_path"])?)?;
        let content =
            fs::read_to_string(&file).map_err(|error| io_error("workspace_read", &file, error))?;
        Ok(
            json!({ "path": file.strip_prefix(&root).unwrap_or(&file).to_string_lossy(), "content": content, "bytes": content.len() }),
        )
    }

    pub(crate) fn workspace_write(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root_for(input, true)?;
        let file = workspace_file_path(&root, required_string(input, &["path", "file_path"])?)?;
        let content = required_string(input, &["content"])?;
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("workspace_write", parent, error))?;
        }
        fs::write(&file, content).map_err(|error| io_error("workspace_write", &file, error))?;
        Ok(
            json!({ "path": file.strip_prefix(&root).unwrap_or(&file).to_string_lossy(), "bytes_written": content.len() }),
        )
    }

    pub(crate) fn workspace_edit(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root_for(input, true)?;
        let file = workspace_file_path(&root, required_string(input, &["path", "file_path"])?)?;
        let old = required_string(input, &["old_string", "old", "search"])?;
        let new = required_string(input, &["new_string", "new", "replace"])?;
        let content =
            fs::read_to_string(&file).map_err(|error| io_error("workspace_edit", &file, error))?;
        let count = content.matches(old).count();
        if count != 1 {
            return Err(validation_error(
                "old_string",
                "workspace_edit requires old_string to match exactly once",
                json!({ "matches": count }),
            ));
        }
        let updated = content.replacen(old, new, 1);
        fs::write(&file, updated).map_err(|error| io_error("workspace_edit", &file, error))?;
        Ok(
            json!({ "path": file.strip_prefix(&root).unwrap_or(&file).to_string_lossy(), "replacements": 1 }),
        )
    }

    pub(crate) fn workspace_grep(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root(input)?;
        let pattern = required_string(input, &["pattern", "query"])?;
        let include = optional_string(input, &["include", "file_pattern"]);
        let path = optional_string(input, &["path", "directory"]).unwrap_or(".");
        let search_path = workspace_file_path(&root, path)?;
        let mut args = vec!["-n".to_string(), "--color=never".to_string()];
        if let Some(include) = include {
            args.push("--glob".to_string());
            args.push(include.to_string());
        }
        args.push(pattern.to_string());
        args.push(search_path.to_string_lossy().to_string());
        let output = Command::new("rg")
            .args(&args)
            .current_dir(&root)
            .output()
            .map_err(|error| command_spawn_error("workspace_grep", error))?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(command_error("workspace_grep", output));
        }
        let matches = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| json!({ "line": line }))
            .collect::<Vec<_>>();
        Ok(json!({ "matches": matches }))
    }

    pub(crate) fn workspace_apply_patch(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root(input)?;
        let patch = required_string(input, &["patch", "diff"])?;
        let mut child = Command::new("git")
            .args(["apply", "--whitespace=nowarn", "-"])
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| command_spawn_error("workspace_apply_patch", error))?;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(patch.as_bytes())
            .map_err(|error| AgentTaskDiagnostic {
                class: "agent_tool.io".to_string(),
                message: error.to_string(),
                data: json!({ "operation": "workspace_apply_patch" }),
            })?;
        let output = child
            .wait_with_output()
            .map_err(|error| command_spawn_error("workspace_apply_patch", error))?;
        if !output.status.success() {
            return Err(command_error("workspace_apply_patch", output));
        }
        Ok(json!({ "applied": true }))
    }

    pub(crate) fn workspace_git_status(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        to_value(git::status_at(
            component_id(input),
            workspace_path_for(input, true).as_deref(),
        ))
    }

    pub(crate) fn workspace_git_commit(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let message = required_string(input, &["message"])?;
        let files =
            optional_string_array(input, "paths").or_else(|| optional_string_array(input, "files"));
        to_value(git::commit_at(
            component_id(input),
            Some(message),
            git::CommitOptions {
                staged_only: bool_input(input, "staged_only"),
                files,
                exclude: None,
                amend: false,
            },
            workspace_path_for(input, true).as_deref(),
        ))
    }

    pub(crate) fn workspace_git_push(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        to_value(git::push_at(
            component_id(input),
            git::PushOptions {
                tags: bool_input(input, "tags"),
                force_with_lease: bool_input(input, "force_with_lease"),
                remote_url: optional_string(input, &["remote_url"]).map(str::to_string),
                token: None,
                refspec: optional_string(input, &["refspec"]).map(str::to_string),
                strip_extraheader: bool_input(input, "strip_extraheader"),
            },
            workspace_path_for(input, true).as_deref(),
        ))
    }

    pub(crate) fn workspace_git_diff(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root_for(input, true)?;
        let output = Command::new("git")
            .args(["diff"])
            .current_dir(&root)
            .output()
            .map_err(|error| command_spawn_error("workspace_git_diff", error))?;
        if !output.status.success() {
            return Err(command_error("workspace_git_diff", output));
        }
        Ok(json!({ "diff": String::from_utf8_lossy(&output.stdout).to_string() }))
    }

    pub(crate) fn workspace_git_add(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let root = workspace_root_for(input, true)?;
        let paths = optional_string_array(input, "paths").unwrap_or_else(|| vec![".".to_string()]);
        let mut args = vec!["add".to_string(), "--".to_string()];
        args.extend(paths.clone());
        let output = Command::new("git")
            .args(&args)
            .current_dir(&root)
            .output()
            .map_err(|error| command_spawn_error("workspace_git_add", error))?;
        if !output.status.success() {
            return Err(command_error("workspace_git_add", output));
        }
        Ok(json!({ "paths": paths }))
    }

    pub(crate) fn workspace_worktree_add(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let repo = required_string(input, &["repo", "component_id", "name"])?;
        let branch = required_string(input, &["branch"])?;
        to_value(worktree::create(worktree::WorktreeCreateOptions {
            component_id: component_slug(repo).to_string(),
            branch: branch.to_string(),
            from: optional_string(input, &["from", "base_ref"]).map(str::to_string),
            task_url: optional_string(input, &["task_url"]).map(str::to_string),
            run_id: optional_string(input, &["run_id", "task_ref"]).map(str::to_string),
            cleanup_policy: None,
        }))
    }

    pub(crate) fn github_issue_get(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let repo = required_string(input, &["repo"])?;
        let number = required_u64(input, &["number", "issue_number"])?;
        run_gh_json(&[
            "issue",
            "view",
            &number.to_string(),
            "-R",
            repo,
            "--json",
            "number,title,body,url,state,labels",
        ])
    }

    pub(crate) fn github_issue_create(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let repo = required_string(input, &["repo"])?;
        let title = required_string(input, &["title"])?;
        let body = required_string(input, &["body"])?;
        let mut args = vec![
            "issue".to_string(),
            "create".to_string(),
            "-R".to_string(),
            repo.to_string(),
            "--title".to_string(),
            title.to_string(),
        ];
        let mut body_files = Vec::new();
        git::push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", body).map_err(
            |error| validation_error("github_body_file", &error.to_string(), Value::Null),
        )?;
        for label in optional_string_array(input, "labels").unwrap_or_default() {
            args.push("--label".to_string());
            args.push(label);
        }
        run_gh_url(&args, "issue.create")
    }

    pub(crate) fn github_pulls_list(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let repo = required_string(input, &["repo"])?;
        let limit = optional_u64(input, &["limit"]).unwrap_or(30).to_string();
        run_gh_json(&[
            "pr",
            "list",
            "-R",
            repo,
            "--limit",
            &limit,
            "--json",
            "number,title,url,state,baseRefName,headRefName",
        ])
    }

    pub(crate) fn github_pull_create(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let repo = required_string(input, &["repo"])?;
        let title = required_string(input, &["title"])?;
        let body = required_string(input, &["body"])?;
        let base = required_string(input, &["base"])?;
        let head = required_string(input, &["head", "branch"])?;
        let mut args = vec![
            "pr".to_string(),
            "create".to_string(),
            "-R".to_string(),
            repo.to_string(),
            "--base".to_string(),
            base.to_string(),
            "--head".to_string(),
            head.to_string(),
            "--title".to_string(),
            title.to_string(),
        ];
        let mut body_files = Vec::new();
        git::push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", body).map_err(
            |error| validation_error("github_body_file", &error.to_string(), Value::Null),
        )?;
        if bool_input(input, "draft") {
            args.push("--draft".to_string());
        }
        run_gh_url(&args, "pr.create")
    }

    pub(crate) fn github_pull_comment(
        input: &serde_json::Map<String, Value>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        let repo = required_string(input, &["repo"])?;
        let number = required_u64(input, &["number", "pr", "pull_number"])?;
        let body = required_string(input, &["body", "comment"])?;
        let mut args = vec![
            "pr".to_string(),
            "comment".to_string(),
            number.to_string(),
            "-R".to_string(),
            repo.to_string(),
        ];
        let mut body_files = Vec::new();
        git::push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", body).map_err(
            |error| validation_error("github_body_file", &error.to_string(), Value::Null),
        )?;
        run_gh_url(&args, "pr.comment")
    }
}

mod gh_helpers {
    use super::*;

    pub(crate) fn run_gh_json(args: &[&str]) -> Result<Value, AgentTaskDiagnostic> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .map_err(|error| command_spawn_error("github", error))?;
        if !output.status.success() {
            return Err(command_error("github", output));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| validation_error("github_json", &error.to_string(), Value::Null))
    }

    pub(crate) fn run_gh_url(args: &[String], action: &str) -> Result<Value, AgentTaskDiagnostic> {
        let output = Command::new("gh")
            .args(args)
            .output()
            .map_err(|error| command_spawn_error(action, error))?;
        if !output.status.success() {
            return Err(command_error(action, output));
        }
        Ok(json!({ "action": action, "url": String::from_utf8_lossy(&output.stdout).trim() }))
    }
}

mod workspace_paths {
    use super::*;

    pub(crate) fn workspace_root(
        input: &serde_json::Map<String, Value>,
    ) -> Result<PathBuf, AgentTaskDiagnostic> {
        workspace_root_for(input, false)
    }

    pub(crate) fn workspace_root_for(
        input: &serde_json::Map<String, Value>,
        prefer_worktree: bool,
    ) -> Result<PathBuf, AgentTaskDiagnostic> {
        let path = workspace_path_for(input, prefer_worktree).ok_or_else(|| validation_error(
            "path",
            "workspace tool requires path/workspace_path/root, or a repo/name/component_id resolvable by Homeboy",
            Value::Null,
        ))?;
        fs::canonicalize(&path).map_err(|error| io_error("workspace_root", Path::new(&path), error))
    }

    pub(crate) fn workspace_path_for(
        input: &serde_json::Map<String, Value>,
        prefer_worktree: bool,
    ) -> Option<String> {
        optional_string(input, &["workspace_path", "root"])
            .map(str::to_string)
            .or_else(|| {
                component_id(input).and_then(|id| {
                    if prefer_worktree {
                        if let Some(path) = latest_active_worktree_path(id) {
                            return Some(path);
                        }
                    }
                    git::status_at(Some(id), None)
                        .ok()
                        .map(|output| output.path)
                })
            })
    }

    pub(crate) fn latest_active_worktree_path(component_id: &str) -> Option<String> {
        worktree::list()
            .ok()?
            .worktrees
            .into_iter()
            .filter(|record| {
                record.component_id == component_id
                    && record.state == worktree::TaskWorktreeState::Active
            })
            .max_by(|left, right| left.created_at.cmp(&right.created_at))
            .map(|record| record.worktree_path)
    }

    pub(crate) fn workspace_file_path(
        root: &Path,
        path: &str,
    ) -> Result<PathBuf, AgentTaskDiagnostic> {
        let joined = root.join(path);
        let normalized = normalize_path(&joined);
        if !normalized.starts_with(root) {
            return Err(validation_error(
                "path",
                "path must stay inside workspace root",
                json!({ "path": path }),
            ));
        }
        Ok(normalized)
    }

    pub(crate) fn normalize_path(path: &Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                std::path::Component::CurDir => {}
                other => normalized.push(other.as_os_str()),
            }
        }
        normalized
    }
}

mod input_helpers {
    use super::*;

    pub(crate) fn component_id(input: &serde_json::Map<String, Value>) -> Option<&str> {
        optional_string(input, &["component_id", "name", "repo"]).map(component_slug)
    }

    pub(crate) fn component_slug(value: &str) -> &str {
        value.rsplit('/').next().unwrap_or(value)
    }

    pub(crate) fn required_string<'a>(
        input: &'a serde_json::Map<String, Value>,
        keys: &[&str],
    ) -> Result<&'a str, AgentTaskDiagnostic> {
        optional_string(input, keys).ok_or_else(|| {
            validation_error(
                keys[0],
                "missing required string field",
                json!({ "accepted_keys": keys }),
            )
        })
    }

    pub(crate) fn optional_string<'a>(
        input: &'a serde_json::Map<String, Value>,
        keys: &[&str],
    ) -> Option<&'a str> {
        keys.iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str))
    }

    pub(crate) fn required_u64(
        input: &serde_json::Map<String, Value>,
        keys: &[&str],
    ) -> Result<u64, AgentTaskDiagnostic> {
        optional_u64(input, keys).ok_or_else(|| {
            validation_error(
                keys[0],
                "missing required integer field",
                json!({ "accepted_keys": keys }),
            )
        })
    }

    pub(crate) fn optional_u64(
        input: &serde_json::Map<String, Value>,
        keys: &[&str],
    ) -> Option<u64> {
        keys.iter()
            .find_map(|key| input.get(*key).and_then(Value::as_u64))
    }

    pub(crate) fn optional_string_array(
        input: &serde_json::Map<String, Value>,
        key: &str,
    ) -> Option<Vec<String>> {
        input.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
    }

    pub(crate) fn bool_input(input: &serde_json::Map<String, Value>, key: &str) -> bool {
        input.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    pub(crate) fn to_value<T: Serialize>(
        result: crate::core::Result<T>,
    ) -> Result<Value, AgentTaskDiagnostic> {
        result
            .map_err(|error| AgentTaskDiagnostic {
                class: "agent_tool.homeboy_error".to_string(),
                message: error.to_string(),
                data: Value::Null,
            })
            .and_then(|output| {
                serde_json::to_value(output).map_err(|error| {
                    validation_error("serialization", &error.to_string(), Value::Null)
                })
            })
    }
}

mod errors {
    use super::*;

    pub(crate) fn validation_error(field: &str, message: &str, data: Value) -> AgentTaskDiagnostic {
        AgentTaskDiagnostic {
            class: "agent_tool.validation".to_string(),
            message: message.to_string(),
            data: json!({ "field": field, "details": data }),
        }
    }

    pub(crate) fn io_error(
        operation: &str,
        path: &Path,
        error: std::io::Error,
    ) -> AgentTaskDiagnostic {
        AgentTaskDiagnostic {
            class: "agent_tool.io".to_string(),
            message: error.to_string(),
            data: json!({ "operation": operation, "path": path.to_string_lossy() }),
        }
    }

    pub(crate) fn command_spawn_error(
        operation: &str,
        error: std::io::Error,
    ) -> AgentTaskDiagnostic {
        AgentTaskDiagnostic {
            class: "agent_tool.command_spawn".to_string(),
            message: error.to_string(),
            data: json!({ "operation": operation }),
        }
    }

    pub(crate) fn command_error(
        operation: &str,
        output: std::process::Output,
    ) -> AgentTaskDiagnostic {
        // The dispatched command's stdout/stderr are unbounded; bound the retained
        // bytes (keeping the trailing tail) with truncation metadata so a
        // pathological command cannot force an arbitrarily large failure payload
        // into the diagnostic (#5363).
        let (stdout, stdout_capture) =
            bound_captured_stream(&output.stdout, COMMAND_CAPTURE_LIMIT_BYTES);
        let (stderr, stderr_capture) =
            bound_captured_stream(&output.stderr, COMMAND_CAPTURE_LIMIT_BYTES);
        AgentTaskDiagnostic {
            class: "agent_tool.command_failed".to_string(),
            message: format!("{} failed", operation),
            data: json!({
                "operation": operation,
                "exit_code": output.status.code(),
                "stdout": stdout,
                "stderr": stderr,
                "stdout_capture": {
                    "limit_bytes": stdout_capture.limit_bytes,
                    "seen_bytes": stdout_capture.seen_bytes,
                    "retained_bytes": stdout_capture.retained_bytes,
                    "truncated": stdout_capture.truncated,
                },
                "stderr_capture": {
                    "limit_bytes": stderr_capture.limit_bytes,
                    "seen_bytes": stderr_capture.seen_bytes,
                    "retained_bytes": stderr_capture.retained_bytes,
                    "truncated": stderr_capture.truncated,
                },
            }),
        }
    }
}

pub(crate) use capture::*;
pub use dispatch::*;
pub(crate) use errors::*;
pub(crate) use gh_helpers::*;
pub(crate) use input_helpers::*;
pub(crate) use results::*;
pub(crate) use tools::*;
pub(crate) use workspace_paths::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::core::agent_task::{
        AgentToolPolicyRule, AGENT_TOOL_POLICY_SCHEMA, AGENT_TOOL_REQUEST_SCHEMA,
    };

    #[test]
    fn bound_captured_stream_retains_full_source_within_limit() {
        let (text, capture) = bound_captured_stream(b"all good", 64);
        assert_eq!(text, "all good");
        assert_eq!(capture.seen_bytes, 8);
        assert_eq!(capture.retained_bytes, 8);
        assert_eq!(capture.limit_bytes, 64);
        assert!(!capture.truncated);
    }

    #[test]
    fn bound_captured_stream_keeps_trailing_tail_when_truncated() {
        let blob = format!("{}TAIL-ERR", "x".repeat(100));
        let (text, capture) = bound_captured_stream(blob.as_bytes(), 8);
        assert_eq!(capture.limit_bytes, 8);
        assert_eq!(capture.seen_bytes, blob.len());
        assert_eq!(capture.retained_bytes, 8);
        assert!(capture.truncated);
        assert_eq!(text, "TAIL-ERR");
    }

    #[test]
    fn command_error_bounds_oversized_streams_with_truncation_metadata() {
        use std::os::unix::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: vec![b'a'; COMMAND_CAPTURE_LIMIT_BYTES + 4096],
            stderr: vec![b'b'; COMMAND_CAPTURE_LIMIT_BYTES + 4096],
        };
        let diagnostic = command_error("workspace_apply_patch", output);
        let data = &diagnostic.data;
        assert_eq!(
            data["stdout"].as_str().map(str::len),
            Some(COMMAND_CAPTURE_LIMIT_BYTES)
        );
        assert_eq!(
            data["stderr"].as_str().map(str::len),
            Some(COMMAND_CAPTURE_LIMIT_BYTES)
        );
        assert_eq!(data["stdout_capture"]["truncated"], json!(true));
        assert_eq!(
            data["stdout_capture"]["retained_bytes"],
            json!(COMMAND_CAPTURE_LIMIT_BYTES)
        );
        assert_eq!(
            data["stdout_capture"]["seen_bytes"],
            json!(COMMAND_CAPTURE_LIMIT_BYTES + 4096)
        );
        assert_eq!(data["stderr_capture"]["truncated"], json!(true));
    }

    #[derive(Debug, Clone, Copy)]
    struct EchoDispatcher;

    impl AgentToolControlPlaneDispatcher for EchoDispatcher {
        fn dispatch(&self, request: &AgentToolRequest) -> AgentToolResult {
            AgentToolResult {
                schema: AGENT_TOOL_RESULT_SCHEMA.to_string(),
                request_id: request.request_id.clone(),
                task_id: request.task_id.clone(),
                tool: request.tool.clone(),
                status: AgentToolResultStatus::Succeeded,
                output: json!({ "token": "secret-output", "safe": true }),
                diagnostics: Vec::new(),
                metadata: json!({ "authorization": "Bearer result-secret" }),
            }
        }
    }

    fn request(tool: &str) -> AgentToolRequest {
        AgentToolRequest {
            schema: AGENT_TOOL_REQUEST_SCHEMA.to_string(),
            request_id: "request-1".to_string(),
            task_id: "task-1".to_string(),
            tool: tool.to_string(),
            input: json!({ "token": "secret-input", "safe": true }),
            timeout_ms: None,
            metadata: json!({ "password": "secret-metadata" }),
        }
    }

    fn policy(default_location: AgentToolExecutionLocation) -> AgentToolPolicy {
        AgentToolPolicy {
            schema: AGENT_TOOL_POLICY_SCHEMA.to_string(),
            default_location,
            tools: BTreeMap::new(),
        }
    }

    #[test]
    fn tool_policy_selects_explicit_route_over_default() {
        let mut policy = policy(AgentToolExecutionLocation::Disabled);
        policy.tools.insert(
            "lookup".to_string(),
            AgentToolPolicyRule {
                execution_location: AgentToolExecutionLocation::ControlPlane,
                timeout_ms: Some(250),
                reason: Some("test route".to_string()),
            },
        );

        let outcome = dispatch_agent_tool_request(&policy, &request("lookup"), &EchoDispatcher);

        assert_eq!(outcome.location, AgentToolExecutionLocation::ControlPlane);
        assert_eq!(outcome.result.status, AgentToolResultStatus::Succeeded);
    }

    #[test]
    fn tool_policy_is_disabled_by_default() {
        let outcome = dispatch_agent_tool_request(
            &AgentToolPolicy::default(),
            &request("lookup"),
            &EchoDispatcher,
        );

        assert_eq!(outcome.location, AgentToolExecutionLocation::Disabled);
        assert_eq!(outcome.result.status, AgentToolResultStatus::Denied);
        assert_eq!(outcome.result.diagnostics[0].class, "agent_tool.disabled");
    }

    #[test]
    fn tool_dispatch_evidence_redacts_request_and_result() {
        let outcome = dispatch_agent_tool_request(
            &policy(AgentToolExecutionLocation::ControlPlane),
            &request("lookup"),
            &EchoDispatcher,
        );

        assert_eq!(outcome.evidence.schema, AGENT_TOOL_DISPATCH_EVIDENCE_SCHEMA);
        assert_eq!(outcome.evidence.request.input["token"], "[REDACTED]");
        assert_eq!(outcome.evidence.request.input["safe"], true);
        assert_eq!(outcome.evidence.request.metadata["password"], "[REDACTED]");
        assert_eq!(outcome.evidence.result.output["token"], "[REDACTED]");
        assert_eq!(
            outcome.evidence.result.metadata["authorization"],
            "[REDACTED]"
        );
    }

    #[test]
    fn unsupported_control_plane_dispatch_returns_explicit_diagnostic() {
        let outcome = dispatch_agent_tool_request(
            &policy(AgentToolExecutionLocation::ControlPlane),
            &request("lookup"),
            &UnsupportedAgentToolControlPlaneDispatcher,
        );

        assert_eq!(outcome.location, AgentToolExecutionLocation::ControlPlane);
        assert_eq!(outcome.result.status, AgentToolResultStatus::Failed);
        assert_eq!(
            outcome.result.diagnostics[0].class,
            "agent_tool.control_plane_dispatch_unsupported"
        );
        assert_eq!(
            outcome.evidence.result.diagnostics[0].class,
            "agent_tool.control_plane_dispatch_unsupported"
        );
    }
}
