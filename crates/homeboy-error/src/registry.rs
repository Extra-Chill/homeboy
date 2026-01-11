use crate::ErrorCode;

pub fn all_codes() -> &'static [ErrorCode] {
    &[
        ErrorCode::ConfigMissingKey,
        ErrorCode::ConfigInvalidJson,
        ErrorCode::ConfigInvalidValue,
        ErrorCode::ValidationMissingArgument,
        ErrorCode::ValidationInvalidArgument,
        ErrorCode::ValidationInvalidJson,
        ErrorCode::ValidationUnknownErrorCode,
        ErrorCode::ProjectNotFound,
        ErrorCode::ProjectNoActive,
        ErrorCode::ServerNotFound,
        ErrorCode::ComponentNotFound,
        ErrorCode::ModuleNotFound,
        ErrorCode::SshServerInvalid,
        ErrorCode::SshIdentityFileNotFound,
        ErrorCode::SshAuthFailed,
        ErrorCode::SshConnectFailed,
        ErrorCode::RemoteCommandFailed,
        ErrorCode::RemoteCommandTimeout,
        ErrorCode::DeployNoComponentsConfigured,
        ErrorCode::DeployBuildFailed,
        ErrorCode::DeployUploadFailed,
        ErrorCode::GitCommandFailed,
        ErrorCode::InternalIoError,
        ErrorCode::InternalJsonError,
        ErrorCode::InternalUnexpected,
    ]
}

pub fn parse_code(code: &str) -> Option<ErrorCode> {
    all_codes()
        .iter()
        .copied()
        .find(|candidate| candidate.as_str() == code)
}
