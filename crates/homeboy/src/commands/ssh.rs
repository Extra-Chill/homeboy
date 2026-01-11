use clap::Args;
use homeboy_core::config::{ConfigManager, ProjectConfiguration, ProjectRecord, ServerConfig};
use homeboy_core::ssh::SshClient;
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct SshArgs {
    /// Project ID or server ID (project wins when both exist)
    pub id: Option<String>,

    /// Force project resolution
    #[arg(long, conflicts_with_all = ["server", "id"])]
    pub project: Option<String>,

    /// Force server resolution
    #[arg(long, conflicts_with_all = ["project", "id"])]
    pub server: Option<String>,

    /// Command to execute (omit for interactive shell)
    pub command: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshOutput {
    pub resolved_type: String,
    pub project_id: Option<String>,
    pub server_id: String,
    pub command: Option<String>,
}

pub fn run(args: SshArgs) -> CmdResult<SshOutput> {
    run_with_loaders_and_executor(
        args,
        ConfigManager::load_project_record,
        ConfigManager::load_server,
        execute_interactive,
    )
}

fn run_with_loaders_and_executor(
    args: SshArgs,
    project_loader: fn(&str) -> homeboy_core::Result<ProjectRecord>,
    server_loader: fn(&str) -> homeboy_core::Result<ServerConfig>,
    executor: fn(&ServerConfig, &str, Option<&str>) -> homeboy_core::Result<i32>,
) -> CmdResult<SshOutput> {
    let (resolved_type, project_id, server_id, server) =
        resolve_context(&args, project_loader, server_loader)?;

    if !server.is_valid() {
        return Err(homeboy_core::Error::Other(
            "Server is not properly configured".to_string(),
        ));
    }

    let exit_code = executor(&server, &server_id, args.command.as_deref())?;

    Ok((
        SshOutput {
            resolved_type,
            project_id,
            server_id,
            command: args.command,
        },
        exit_code,
    ))
}

fn execute_interactive(
    server: &ServerConfig,
    server_id: &str,
    command: Option<&str>,
) -> homeboy_core::Result<i32> {
    let client = SshClient::from_server(server, server_id)?;
    Ok(client.execute_interactive(command))
}

fn resolve_context(
    args: &SshArgs,
    project_loader: fn(&str) -> homeboy_core::Result<ProjectRecord>,
    server_loader: fn(&str) -> homeboy_core::Result<ServerConfig>,
) -> homeboy_core::Result<(String, Option<String>, String, ServerConfig)> {
    if let Some(project_id) = &args.project {
        let record = project_loader(project_id)?;
        let (server_id, server) = resolve_from_loaded_project(&record.config, server_loader)?;
        return Ok(("project".to_string(), Some(record.id), server_id, server));
    }

    if let Some(server_id) = &args.server {
        let server = server_loader(server_id)?;
        return Ok(("server".to_string(), None, server_id.clone(), server));
    }

    let id = args.id.as_ref().ok_or_else(|| {
        homeboy_core::Error::Other("Project ID or server ID is required".to_string())
    })?;

    if let Ok(record) = project_loader(id) {
        let (server_id, server) = resolve_from_loaded_project(&record.config, server_loader)?;
        return Ok(("project".to_string(), Some(record.id), server_id, server));
    }

    if let Ok(server) = server_loader(id) {
        return Ok(("server".to_string(), None, id.to_string(), server));
    }

    Err(homeboy_core::Error::Other(format!(
        "No project or server found with id '{}'",
        id
    )))
}

fn resolve_from_loaded_project(
    project: &ProjectConfiguration,
    server_loader: fn(&str) -> homeboy_core::Result<ServerConfig>,
) -> homeboy_core::Result<(String, ServerConfig)> {
    let server_id = project.server_id.clone().ok_or_else(|| {
        homeboy_core::Error::Other("Server not configured for project".to_string())
    })?;

    let server = server_loader(&server_id)?;

    Ok((server_id, server))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(_id: &str) -> ServerConfig {
        ServerConfig {
            name: "Test".to_string(),
            host: "example.com".to_string(),
            user: "user".to_string(),
            port: 22,
            identity_file: None,
        }
    }

    fn project(id: &str, server_id: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: id.to_string(),
            config: ProjectConfiguration {
                name: String::new(),
                domain: String::new(),
                project_type: "wordpress".to_string(),
                server_id: server_id.map(|s| s.to_string()),
                base_path: None,
                table_prefix: None,
                remote_files: homeboy_core::config::RemoteFileConfig::default(),
                remote_logs: homeboy_core::config::RemoteLogConfig::default(),
                database: homeboy_core::config::DatabaseConfig::default(),
                local_environment: homeboy_core::config::LocalEnvironmentConfig::default(),
                tools: homeboy_core::config::ToolsConfig::default(),
                api: homeboy_core::config::ApiConfig::default(),
                changelog_next_section_label: None,
                changelog_next_section_aliases: None,
                sub_targets: vec![],
                shared_tables: vec![],
                component_ids: vec![],
                table_groupings: vec![],
                component_groupings: vec![],
                protected_table_patterns: vec![],
                unlocked_table_patterns: vec![],
            },
        }
    }

    fn noop_executor(
        _server: &ServerConfig,
        _server_id: &str,
        _command: Option<&str>,
    ) -> homeboy_core::Result<i32> {
        Ok(0)
    }

    #[test]
    fn resolves_project_first_when_both_exist() {
        let args = SshArgs {
            id: Some("alpha".to_string()),
            project: None,
            server: None,
            command: Some("pwd".to_string()),
        };

        let result = run_with_loaders_and_executor(
            args,
            |id| match id {
                "alpha" => Ok(project("alpha", Some("alpha"))),
                _ => Err(homeboy_core::Error::Other("no project".to_string())),
            },
            |id| Ok(server(id)),
            noop_executor,
        )
        .unwrap();

        assert_eq!(result.0.resolved_type, "project");
        assert_eq!(result.0.project_id.as_deref(), Some("alpha"));
        assert_eq!(result.0.server_id, "alpha");
    }

    #[test]
    fn resolves_server_when_project_missing() {
        let args = SshArgs {
            id: Some("cloudways".to_string()),
            project: None,
            server: None,
            command: None,
        };

        let result = run_with_loaders_and_executor(
            args,
            |_id| Err(homeboy_core::Error::Other("no project".to_string())),
            |id| match id {
                "cloudways" => Ok(server(id)),
                _ => Err(homeboy_core::Error::Other("no server".to_string())),
            },
            noop_executor,
        )
        .unwrap();

        assert_eq!(result.0.resolved_type, "server");
        assert!(result.0.project_id.is_none());
        assert_eq!(result.0.server_id, "cloudways");
    }

    #[test]
    fn server_flag_forces_server_even_if_project_exists() {
        let args = SshArgs {
            id: None,
            project: None,
            server: Some("alpha".to_string()),
            command: Some("uptime".to_string()),
        };

        let result = run_with_loaders_and_executor(
            args,
            |_id| Ok(project("alpha", Some("alpha"))),
            |id| Ok(server(id)),
            noop_executor,
        )
        .unwrap();

        assert_eq!(result.0.resolved_type, "server");
        assert!(result.0.project_id.is_none());
        assert_eq!(result.0.server_id, "alpha");
    }

    #[test]
    fn returns_error_when_neither_project_nor_server_found() {
        let args = SshArgs {
            id: Some("missing".to_string()),
            project: None,
            server: None,
            command: None,
        };

        let error = run_with_loaders_and_executor(
            args,
            |_id| Err(homeboy_core::Error::Other("no project".to_string())),
            |_id| Err(homeboy_core::Error::Other("no server".to_string())),
            noop_executor,
        )
        .unwrap_err();

        assert!(error.to_string().contains("No project or server found"));
    }
}
