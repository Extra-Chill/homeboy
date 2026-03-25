//! error_code — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use serde_json::Value;


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
