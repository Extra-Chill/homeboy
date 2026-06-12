use crate::core::engine::shell;
use crate::core::error::{Error, Result};
use crate::core::extension;
use crate::core::server::{self, SshClient};

use serde_json::Value;

use super::{Runner, RunnerKind};

pub(super) fn required_extensions_for_command(
    command: &[String],
    explicit: &[String],
) -> Vec<String> {
    let mut extensions = explicit
        .iter()
        .filter(|extension| !extension.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();

    let mut args = command.iter();
    while let Some(arg) = args.next() {
        if arg == "--extension" {
            if let Some(extension) = args.next().filter(|value| !value.trim().is_empty()) {
                push_unique(&mut extensions, extension.to_string());
            }
            continue;
        }
        if let Some(extension) = arg.strip_prefix("--extension=") {
            if !extension.trim().is_empty() {
                push_unique(&mut extensions, extension.to_string());
            }
        }
    }

    extensions
}

fn push_unique(items: &mut Vec<String>, item: String) {
    if !items.contains(&item) {
        items.push(item);
    }
}

pub(super) fn validate_runner_extension_parity(
    runner_id: &str,
    runner: &Runner,
    cwd: &str,
    required_extensions: &[String],
) -> Result<()> {
    for extension_id in required_extensions {
        validate_runner_extension(runner_id, runner, cwd, extension_id)?;
    }

    Ok(())
}

fn validate_runner_extension(
    runner_id: &str,
    runner: &Runner,
    cwd: &str,
    extension_id: &str,
) -> Result<()> {
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let command = format!(
        "cd {} && {} extension show {}",
        shell::quote_path(cwd),
        shell::quote_path(homeboy_path),
        shell::quote_arg(extension_id)
    );
    let output = match runner.kind {
        RunnerKind::Local => server::execute_local_command(&command),
        RunnerKind::Ssh => {
            let client = ssh_client_for_runner_extension_parity(runner)?;
            client.execute(&command)
        }
    };

    if output.success {
        validate_runner_extension_ready(runner_id, homeboy_path, extension_id, &output.stdout)?;
        validate_runner_extension_revision(runner_id, homeboy_path, extension_id, &output.stdout)?;
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' is missing required extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!(
                "Install the extension on the runner before dispatch: {homeboy_path} extension install <source> --id {extension_id}"
            ),
            format!("Remote preflight command failed: {homeboy_path} extension show {extension_id}"),
            extension_parity_diagnostic_tail(&output.stderr, &output.stdout),
        ]),
    ))
}

fn validate_runner_extension_ready(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
) -> Result<()> {
    let Some(status) = remote_extension_ready_status(remote_stdout) else {
        return Ok(());
    };
    if status.ready {
        return Ok(());
    }

    let mut tried = vec![format!("Runner extension ready: false")];
    if let Some(reason) = status.reason.filter(|value| !value.trim().is_empty()) {
        tried.push(format!("Ready reason: {reason}"));
    }
    if let Some(detail) = status.detail.filter(|value| !value.trim().is_empty()) {
        tried.push(format!("Ready detail: {detail}"));
    }

    Err(Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' has unready extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!("Run extension setup on the runner before dispatch: {homeboy_path} extension setup {extension_id}"),
            format!("If setup remains stale, update or relink the extension on the runner: {homeboy_path} extension update {extension_id} or {homeboy_path} extension relink {extension_id} <source>"),
            tried.join("\n"),
        ]),
    ))
}

struct RemoteExtensionReadyStatus {
    ready: bool,
    reason: Option<String>,
    detail: Option<String>,
}

fn remote_extension_ready_status(stdout: &str) -> Option<RemoteExtensionReadyStatus> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let extension = value.get("data").and_then(|data| data.get("extension"))?;
    Some(RemoteExtensionReadyStatus {
        ready: extension.get("ready").and_then(Value::as_bool)?,
        reason: extension
            .get("ready_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        detail: extension
            .get("ready_detail")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn validate_runner_extension_revision(
    runner_id: &str,
    homeboy_path: &str,
    extension_id: &str,
    remote_stdout: &str,
) -> Result<()> {
    let local_revision = extension::read_source_revision(extension_id);
    let remote_revision = remote_extension_source_revision(remote_stdout);
    let Some(local_revision) = local_revision.filter(|revision| !revision.trim().is_empty()) else {
        return Ok(());
    };
    let Some(remote_revision) = remote_revision.filter(|revision| !revision.trim().is_empty())
    else {
        return Err(Error::validation_invalid_argument(
            "runner_extension",
            format!(
                "Runner '{runner_id}' has stale extension parity for '{extension_id}' before command execution"
            ),
            Some(extension_id.to_string()),
            Some(vec![
                format!("Local extension source_revision: {local_revision}"),
                "Runner extension source_revision: <missing>".to_string(),
                format!(
                    "Relink or update the extension on the runner before dispatch: {homeboy_path} extension relink {extension_id} <source>"
                ),
            ]),
        ));
    };

    if local_revision == remote_revision {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "runner_extension",
        format!(
            "Runner '{runner_id}' has stale extension parity for '{extension_id}' before command execution"
        ),
        Some(extension_id.to_string()),
        Some(vec![
            format!("Local extension source_revision: {local_revision}"),
            format!("Runner extension source_revision: {remote_revision}"),
            format!(
                "Relink or update the extension on the runner before dispatch: {homeboy_path} extension relink {extension_id} <source>"
            ),
        ]),
    ))
}

fn remote_extension_source_revision(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    value
        .get("data")
        .and_then(|data| data.get("extension"))
        .and_then(|extension| extension.get("source_revision"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{
        remote_extension_ready_status, remote_extension_source_revision,
        validate_runner_extension_ready, validate_runner_extension_revision,
    };
    use crate::test_support::with_isolated_home;

    use std::fs;

    #[test]
    fn remote_extension_source_revision_reads_extension_show_output() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","source_revision":"abc1234"}}}"#;

        assert_eq!(
            remote_extension_source_revision(stdout).as_deref(),
            Some("abc1234")
        );
    }

    #[test]
    fn remote_extension_ready_status_reads_extension_show_output() {
        let stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","ready":false,"ready_reason":"ready_check_failed","ready_detail":"missing generated asset"}}}"#;
        let status = remote_extension_ready_status(stdout).expect("ready status");

        assert!(!status.ready);
        assert_eq!(status.reason.as_deref(), Some("ready_check_failed"));
        assert_eq!(status.detail.as_deref(), Some("missing generated asset"));
    }

    #[test]
    fn readiness_parity_rejects_unready_runner_extension() {
        let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","ready":false,"ready_reason":"ready_check_failed","ready_detail":"missing generated asset"}}}"#;

        let err =
            validate_runner_extension_ready("homeboy-lab", "homeboy", "wordpress", remote_stdout)
                .expect_err("unready runner extension should fail parity");

        assert!(err.to_string().contains("unready extension parity"));
        assert!(err.details["tried"]
            .to_string()
            .contains("extension setup wordpress"));
        assert!(err.details["tried"]
            .to_string()
            .contains("missing generated asset"));
    }

    #[test]
    fn readiness_parity_accepts_ready_runner_extension() {
        let remote_stdout =
            r#"{"success":true,"data":{"extension":{"id":"wordpress","ready":true}}}"#;

        validate_runner_extension_ready("homeboy-lab", "homeboy", "wordpress", remote_stdout)
            .expect("ready runner extension should pass parity");
    }

    #[test]
    fn revision_parity_rejects_stale_runner_extension() {
        with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/wordpress");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(extension_dir.join(".source-revision"), "local123\n").expect("revision");
            let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress","source_revision":"remote456"}}}"#;

            let err = validate_runner_extension_revision(
                "homeboy-lab",
                "homeboy",
                "wordpress",
                remote_stdout,
            )
            .expect_err("stale runner extension should fail parity");

            assert!(err.to_string().contains("stale extension parity"));
            assert!(err.details["tried"].to_string().contains("local123"));
            assert!(err.details["tried"].to_string().contains("remote456"));
        });
    }

    #[test]
    fn revision_parity_rejects_runner_extension_without_source_revision() {
        with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/wordpress");
            fs::create_dir_all(&extension_dir).expect("extension dir");
            fs::write(extension_dir.join(".source-revision"), "local123\n").expect("revision");
            let remote_stdout = r#"{"success":true,"data":{"extension":{"id":"wordpress"}}}"#;

            let err = validate_runner_extension_revision(
                "homeboy-lab",
                "homeboy",
                "wordpress",
                remote_stdout,
            )
            .expect_err("runner extension without revision should fail parity");

            assert!(err.to_string().contains("stale extension parity"));
            assert!(err.details["tried"].to_string().contains("local123"));
            assert!(err.details["tried"].to_string().contains("<missing>"));
        });
    }
}

fn ssh_client_for_runner_extension_parity(runner: &Runner) -> Result<SshClient> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runners require server_id for runner extension parity preflight",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(runner.env.clone());
    Ok(client)
}

fn extension_parity_diagnostic_tail(stderr: &str, stdout: &str) -> String {
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
        "Runner extension parity preflight produced no diagnostic output.".to_string()
    } else {
        format!("Runner extension parity preflight output:\n{tail}")
    }
}
