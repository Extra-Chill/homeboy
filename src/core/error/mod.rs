use serde::{Deserialize, Serialize};
use serde_json::Value;

fn format_suggestions(suggestions: &[String]) -> String {
    if suggestions.len() == 1 {
        format!("Did you mean: {}?", suggestions[0])
    } else {
        format!("Did you mean: {}?", suggestions.join(", "))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    ConfigMissingKey,
    ConfigInvalidJson,
    ConfigInvalidValue,
    ConfigIdCollision,

    ValidationMissingArgument,
    ValidationInvalidArgument,
    ValidationInvalidJson,
    ValidationMultipleErrors,

    ProjectNotFound,
    ProjectNoActive,
    ServerNotFound,
    ComponentNotFound,
    ComponentNotAttached,
    FleetNotFound,
    ExtensionNotFound,
    DocsTopicNotFound,

    SshServerInvalid,
    SshIdentityFileNotFound,
    SshAuthFailed,
    SshConnectFailed,

    RemoteCommandFailed,
    RemoteCommandTimeout,

    DeployNoComponentsConfigured,
    DeployBuildFailed,
    DeployUploadFailed,

    GitCommandFailed,

    InternalIoError,
    InternalJsonError,
    InternalUnexpected,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::ConfigMissingKey => "config.missing_key",
            ErrorCode::ConfigInvalidJson => "config.invalid_json",
            ErrorCode::ConfigInvalidValue => "config.invalid_value",
            ErrorCode::ConfigIdCollision => "config.id_collision",

            ErrorCode::ValidationMissingArgument => "validation.missing_argument",
            ErrorCode::ValidationInvalidArgument => "validation.invalid_argument",
            ErrorCode::ValidationInvalidJson => "validation.invalid_json",
            ErrorCode::ValidationMultipleErrors => "validation.multiple_errors",

            ErrorCode::ProjectNotFound => "project.not_found",
            ErrorCode::ProjectNoActive => "project.no_active",
            ErrorCode::ServerNotFound => "server.not_found",
            ErrorCode::ComponentNotFound => "component.not_found",
            ErrorCode::ComponentNotAttached => "component.not_attached",
            ErrorCode::FleetNotFound => "fleet.not_found",
            ErrorCode::ExtensionNotFound => "extension.not_found",
            ErrorCode::DocsTopicNotFound => "docs.topic_not_found",

            ErrorCode::SshServerInvalid => "ssh.server_invalid",
            ErrorCode::SshIdentityFileNotFound => "ssh.identity_file_not_found",
            ErrorCode::SshAuthFailed => "ssh.auth_failed",
            ErrorCode::SshConnectFailed => "ssh.connect_failed",

            ErrorCode::RemoteCommandFailed => "remote.command_failed",
            ErrorCode::RemoteCommandTimeout => "remote.command_timeout",

            ErrorCode::DeployNoComponentsConfigured => "deploy.no_components_configured",
            ErrorCode::DeployBuildFailed => "deploy.build_failed",
            ErrorCode::DeployUploadFailed => "deploy.upload_failed",

            ErrorCode::GitCommandFailed => "git.command_failed",

            ErrorCode::InternalIoError => "internal.io_error",
            ErrorCode::InternalJsonError => "internal.json_error",
            ErrorCode::InternalUnexpected => "internal.unexpected",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct Hint {
    pub message: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigMissingKeyDetails {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct ConfigInvalidJsonDetails {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigInvalidValueDetails {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub problem: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigIdCollisionDetails {
    pub id: String,
    pub requested_type: String,
    pub existing_type: String,
}

#[derive(Debug, Serialize)]

pub struct NoActiveProjectDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub hints: Vec<Hint>,
    pub retryable: Option<bool>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Serialize)]

pub struct NotFoundDetails {
    pub id: String,
}

#[derive(Debug, Serialize)]

pub struct MissingArgumentDetails {
    pub args: Vec<String>,
}

#[derive(Debug, Serialize)]

pub struct InvalidArgumentDetails {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tried: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationErrorItem {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct MultipleValidationErrorsDetails {
    pub errors: Vec<ValidationErrorItem>,
}

#[derive(Debug, Serialize)]

pub struct InternalIoErrorDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct InternalJsonErrorDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct TargetDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct RemoteCommandFailedDetails {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub target: TargetDetails,
}

#[derive(Debug, Serialize)]

pub struct SshServerInvalidDetails {
    pub server_id: String,
    pub missing_fields: Vec<String>,
}

#[derive(Debug, Serialize)]

pub struct SshIdentityFileNotFoundDetails {
    pub server_id: String,
    pub identity_file: String,
}

/// Serialize a details struct to JSON Value, falling back to empty object on failure.
fn to_details(details: impl Serialize) -> Value {
    serde_json::to_value(details).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            hints: Vec::new(),
            retryable: None,
        }
    }

    pub fn validation_missing_argument(args: Vec<String>) -> Self {
        let details = to_details(MissingArgumentDetails { args });
        Self::new(
            ErrorCode::ValidationMissingArgument,
            "Missing required argument",
            details,
        )
    }

    pub fn validation_invalid_argument(
        field: impl Into<String>,
        problem: impl Into<String>,
        id: Option<String>,
        tried: Option<Vec<String>>,
    ) -> Self {
        let field_str = field.into();
        let problem_str = problem.into();
        let message = format!("Invalid argument '{}': {}", field_str, problem_str);
        let details = to_details(InvalidArgumentDetails {
            field: field_str,
            problem: problem_str,
            id,
            tried,
        });

        Self::new(ErrorCode::ValidationInvalidArgument, message, details)
    }

    pub fn validation_invalid_json(
        err: serde_json::Error,
        context: Option<String>,
        received: Option<String>,
    ) -> Self {
        let mut details = serde_json::json!({
            "error": err.to_string(),
            "context": context,
        });
        if let Some(received_json) = received {
            details["received"] =
                serde_json::json!(received_json.chars().take(200).collect::<String>());
        }

        Self::new(ErrorCode::ValidationInvalidJson, "Invalid JSON", details)
    }

    pub fn validation_multiple_errors(errors: Vec<ValidationErrorItem>) -> Self {
        let count = errors.len();
        let details = to_details(MultipleValidationErrorsDetails { errors });

        Self::new(
            ErrorCode::ValidationMultipleErrors,
            format!(
                "Found {} validation issue{}",
                count,
                if count == 1 { "" } else { "s" }
            ),
            details,
        )
    }

    /// Generic entity-not-found constructor. All entity-specific variants delegate here.
    pub fn entity_not_found(
        code: ErrorCode,
        entity_type: &str,
        id: impl Into<String>,
        suggestions: Vec<String>,
    ) -> Self {
        let mut err = Self::not_found(code, &format!("{} not found", entity_type), id);
        if !suggestions.is_empty() {
            err = err.with_hint(format_suggestions(&suggestions));
        }
        let list_cmd = entity_type.to_lowercase();
        err.with_hint(format!(
            "Run 'homeboy {} list' to see available {}s",
            list_cmd, list_cmd
        ))
    }

    pub fn project_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ProjectNotFound, "Project", id, suggestions)
    }

    pub fn server_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ServerNotFound, "Server", id, suggestions)
    }

    pub fn component_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ComponentNotFound, "Component", id, suggestions)
    }

    pub fn component_not_attached(
        id: impl Into<String>,
        local_path: impl Into<String>,
        project_suggestion: Option<String>,
    ) -> Self {
        let id = id.into();
        let lp = local_path.into();
        let details = to_details(NotFoundDetails { id: id.clone() });
        let mut err = Self::new(
            ErrorCode::ComponentNotAttached,
            format!(
                "Component '{}' is registered but not attached to any project. Release and deploy require project attachment.",
                id
            ),
            details,
        );
        if let Some(proj) = project_suggestion {
            err = err
                .with_hint(format!(
                    "Attach: homeboy project components attach-path {} {}",
                    proj, lp
                ))
                .with_hint(format!(
                    "If only one project exists, run: homeboy project components attach-path {} {}",
                    proj, lp
                ));
        } else {
            err = err.with_hint(format!(
                "Attach to a project: homeboy project components attach-path <project> {}",
                lp
            ));
        }
        err = err.with_hint("List projects: homeboy project list".to_string());
        err
    }

    pub fn extension_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ExtensionNotFound, "Extension", id, suggestions)
    }

    pub fn fleet_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::FleetNotFound, "Fleet", id, suggestions)
    }

    pub fn docs_topic_not_found(topic: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::DocsTopicNotFound,
            "Documentation topic not found",
            serde_json::json!({ "topic": topic.into() }),
        )
        .with_hint("Run 'homeboy docs list' to see available topics")
        .with_hint("Topics use path format: 'commands/deploy', 'commands/changes'")
    }

    fn not_found(code: ErrorCode, message: &str, id: impl Into<String>) -> Self {
        let details = to_details(NotFoundDetails { id: id.into() });
        Self::new(code, message, details)
    }

    pub fn ssh_server_invalid(server_id: impl Into<String>, missing_fields: Vec<String>) -> Self {
        let details = to_details(SshServerInvalidDetails {
            server_id: server_id.into(),
            missing_fields,
        });

        Self::new(
            ErrorCode::SshServerInvalid,
            "Server is not properly configured",
            details,
        )
    }

    pub fn ssh_identity_file_not_found(
        server_id: impl Into<String>,
        identity_file: impl Into<String>,
    ) -> Self {
        let details = to_details(SshIdentityFileNotFoundDetails {
            server_id: server_id.into(),
            identity_file: identity_file.into(),
        });

        Self::new(
            ErrorCode::SshIdentityFileNotFound,
            "SSH identity file not found",
            details,
        )
    }

    pub fn remote_command_failed(details: RemoteCommandFailedDetails) -> Self {
        let details = to_details(details);

        Self::new(
            ErrorCode::RemoteCommandFailed,
            "Remote command failed",
            details,
        )
    }

    pub fn git_command_failed(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::GitCommandFailed,
            message,
            Value::Object(serde_json::Map::new()),
        )
    }

    pub fn config_missing_key(key: impl Into<String>, path: Option<String>) -> Self {
        let details = to_details(ConfigMissingKeyDetails {
            key: key.into(),
            path,
        });

        Self::new(
            ErrorCode::ConfigMissingKey,
            "Missing required configuration key",
            details,
        )
    }

    pub fn config_invalid_json(path: impl Into<String>, err: serde_json::Error) -> Self {
        let details = to_details(ConfigInvalidJsonDetails {
            path: path.into(),
            error: err.to_string(),
        });

        Self::new(
            ErrorCode::ConfigInvalidJson,
            "Invalid JSON in configuration",
            details,
        )
    }

    pub fn config_invalid_value(
        key: impl Into<String>,
        value: Option<String>,
        problem: impl Into<String>,
    ) -> Self {
        let details = to_details(ConfigInvalidValueDetails {
            key: key.into(),
            value,
            problem: problem.into(),
        });

        Self::new(
            ErrorCode::ConfigInvalidValue,
            "Invalid configuration value",
            details,
        )
    }

    pub fn config_id_collision(
        id: impl Into<String>,
        requested_type: impl Into<String>,
        existing_type: impl Into<String>,
    ) -> Self {
        let existing = existing_type.into();
        let id_str = id.into();
        let details = to_details(ConfigIdCollisionDetails {
            id: id_str.clone(),
            requested_type: requested_type.into(),
            existing_type: existing.clone(),
        });

        Self::new(
            ErrorCode::ConfigIdCollision,
            format!("ID '{}' already exists as a {}", id_str, existing),
            details,
        )
        .with_hint(format!(
            "Run 'homeboy {} rename {} <new-id>' to resolve the collision",
            existing, id_str
        ))
    }

    pub fn project_no_active(config_path: Option<String>) -> Self {
        let details = to_details(NoActiveProjectDetails { config_path });

        Self::new(ErrorCode::ProjectNoActive, "No active project set", details)
    }

    pub fn internal_io(error: impl Into<String>, context: Option<String>) -> Self {
        let details = to_details(InternalIoErrorDetails {
            error: error.into(),
            context,
        });

        Self::new(ErrorCode::InternalIoError, "IO error", details)
    }

    pub fn internal_json(error: impl Into<String>, context: Option<String>) -> Self {
        let details = to_details(InternalJsonErrorDetails {
            error: error.into(),
            context,
        });

        Self::new(ErrorCode::InternalJsonError, "JSON error", details)
    }

    pub fn internal_unexpected(error: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InternalUnexpected,
            error,
            Value::Object(serde_json::Map::new()),
        )
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::config_invalid_value("config", None, message)
    }

    pub fn with_hint(mut self, message: impl Into<String>) -> Self {
        self.hints.push(Hint {
            message: message.into(),
        });
        self
    }

    pub fn with_contextual_hint(self) -> Self {
        match self.code {
            ErrorCode::ComponentNotFound
            | ErrorCode::ProjectNotFound
            | ErrorCode::ProjectNoActive => self.with_hint(
                "Run 'homeboy status --full' to see project context and available components",
            ),
            _ => self,
        }
    }
}
