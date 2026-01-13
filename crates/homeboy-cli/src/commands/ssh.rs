use clap::{Args, Subcommand};
use homeboy::project::{Project, ProjectRecord};
use homeboy::server::{self, Server};
use homeboy::ssh::SshClient;
use serde::Serialize;

use super::CmdResult;

#[derive(Args)]
pub struct SshArgs {
    /// Project ID or server ID (project wins when both exist)
    #[arg(conflicts_with_all = ["project", "server"])]
    pub id: Option<String>,

    /// Force project resolution
    #[arg(long, conflicts_with_all = ["server", "id"])]
    pub project: Option<String>,

    /// Force server resolution
    #[arg(long, conflicts_with_all = ["project", "id"])]
    pub server: Option<String>,

    /// Command to execute (omit for interactive shell)
    pub command: Option<String>,

    #[command(subcommand)]
    pub subcommand: Option<SshSubcommand>,
}

#[derive(Subcommand)]
pub enum SshSubcommand {
    /// List configured SSH server targets
    List,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum SshOutput {
    Connect(SshConnectOutput),
    List(SshListOutput),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectOutput {
    pub resolved_type: String,
    pub project_id: Option<String>,
    pub server_id: String,
    pub command: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshListOutput {
    pub servers: Vec<Server>,
}

pub fn run(args: SshArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<SshOutput> {
    match args.subcommand {
        Some(SshSubcommand::List) => {
            let servers = server::list()?;
            Ok((SshOutput::List(SshListOutput { servers }), 0))
        }
        None => {
            if args.id.is_none() && args.project.is_none() && args.server.is_none() {
                return Err(homeboy::Error::validation_missing_argument(vec![
                    "<id>".to_string(),
                    "--project".to_string(),
                    "--server".to_string(),
                ]));
            }

            run_with_loaders_and_executor(
                args,
                homeboy::project::load_record,
                server::load,
                execute_interactive,
            )
        }
    }
}

fn run_with_loaders_and_executor(
    args: SshArgs,
    project_loader: fn(&str) -> homeboy::Result<ProjectRecord>,
    server_loader: fn(&str) -> homeboy::Result<ServerConfig>,
    executor: fn(&ServerConfig, &str, Option<&str>) -> homeboy::Result<i32>,
) -> CmdResult<SshOutput> {
    let (resolved_type, project_id, server_id, server) =
        resolve_context(&args, project_loader, server_loader)?;

    if !server.is_valid() {
        let mut missing_fields = Vec::new();
        if server.host.is_empty() {
            missing_fields.push("host".to_string());
        }
        if server.user.is_empty() {
            missing_fields.push("user".to_string());
        }

        return Err(homeboy::Error::ssh_server_invalid(
            server_id.clone(),
            missing_fields,
        ));
    }

    let exit_code = executor(&server, &server_id, args.command.as_deref())?;

    Ok((
        SshOutput::Connect(SshConnectOutput {
            resolved_type,
            project_id,
            server_id,
            command: args.command,
        }),
        exit_code,
    ))
}

fn execute_interactive(
    server: &ServerConfig,
    server_id: &str,
    command: Option<&str>,
) -> homeboy::Result<i32> {
    let client = SshClient::from_server(server, server_id)?;
    Ok(client.execute_interactive(command))
}

fn resolve_context(
    args: &SshArgs,
    project_loader: fn(&str) -> homeboy::Result<ProjectRecord>,
    server_loader: fn(&str) -> homeboy::Result<ServerConfig>,
) -> homeboy::Result<(String, Option<String>, String, ServerConfig)> {
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
        homeboy::Error::validation_missing_argument(vec![
            "<id>".to_string(),
            "--project".to_string(),
            "--server".to_string(),
        ])
    })?;

    if let Ok(record) = project_loader(id) {
        let (server_id, server) = resolve_from_loaded_project(&record.config, server_loader)?;
        return Ok(("project".to_string(), Some(record.id), server_id, server));
    }

    if let Ok(server) = server_loader(id) {
        return Ok(("server".to_string(), None, id.to_string(), server));
    }

    Err(homeboy::Error::validation_invalid_argument(
        "id",
        "No matching project or server",
        Some(id.to_string()),
        Some(vec!["project".to_string(), "server".to_string()]),
    ))
}

fn resolve_from_loaded_project(
    project: &ProjectConfiguration,
    server_loader: fn(&str) -> homeboy::Result<ServerConfig>,
) -> homeboy::Result<(String, ServerConfig)> {
    let server_id = project.server_id.clone().ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "project.serverId",
            "Server not configured for project",
            None,
            None,
        )
    })?;

    let server = server_loader(&server_id)?;

    Ok((server_id, server))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str) -> ServerConfig {
        ServerConfig {
            id: id.to_string(),
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
                modules: vec![],
                scoped_modules: None,
                server_id: server_id.map(|s| s.to_string()),
                base_path: None,
                table_prefix: None,
                remote_files: homeboy::config::RemoteFileConfig::default(),
                remote_logs: homeboy::config::RemoteLogConfig::default(),
                database: homeboy::config::DatabaseConfig::default(),
                tools: homeboy::config::ToolsConfig::default(),
                api: homeboy::config::ApiConfig::default(),
                changelog_next_section_label: None,
                changelog_next_section_aliases: None,
                sub_targets: vec![],
                shared_tables: vec![],
                component_ids: vec![],
            },
        }
    }

    fn noop_executor(
        _server: &ServerConfig,
        _server_id: &str,
        _command: Option<&str>,
    ) -> homeboy::Result<i32> {
        Ok(0)
    }

    #[test]
    fn resolves_project_first_when_both_exist() {
        let args = SshArgs {
            id: Some("alpha".to_string()),
            project: None,
            server: None,
            command: Some("pwd".to_string()),
            subcommand: None,
        };

        let result = run_with_loaders_and_executor(
            args,
            |id| match id {
                "alpha" => Ok(project("alpha", Some("alpha"))),
                _ => Err(homeboy::Error::project_not_found("missing")),
            },
            |id| Ok(server(id)),
            noop_executor,
        )
        .unwrap();

        let data = match result.0 {
            SshOutput::Connect(data) => data,
            _ => panic!("expected connect output"),
        };

        assert_eq!(data.resolved_type, "project");
        assert_eq!(data.project_id.as_deref(), Some("alpha"));
        assert_eq!(data.server_id, "alpha");
    }

    #[test]
    fn resolves_server_when_project_missing() {
        let args = SshArgs {
            id: Some("prod-server".to_string()),
            project: None,
            server: None,
            command: None,
            subcommand: None,
        };

        let result = run_with_loaders_and_executor(
            args,
            |_id| Err(homeboy::Error::project_not_found("missing")),
            |id| match id {
                "prod-server" => Ok(server(id)),
                _ => Err(homeboy::Error::server_not_found("missing")),
            },
            noop_executor,
        )
        .unwrap();

        let data = match result.0 {
            SshOutput::Connect(data) => data,
            _ => panic!("expected connect output"),
        };

        assert_eq!(data.resolved_type, "server");
        assert!(data.project_id.is_none());
        assert_eq!(data.server_id, "prod-server");
    }

    #[test]
    fn server_flag_forces_server_even_if_project_exists() {
        let args = SshArgs {
            id: None,
            project: None,
            server: Some("alpha".to_string()),
            command: Some("uptime".to_string()),
            subcommand: None,
        };

        let result = run_with_loaders_and_executor(
            args,
            |_id| Ok(project("alpha", Some("alpha"))),
            |id| Ok(server(id)),
            noop_executor,
        )
        .unwrap();

        let data = match result.0 {
            SshOutput::Connect(data) => data,
            _ => panic!("expected connect output"),
        };

        assert_eq!(data.resolved_type, "server");
        assert!(data.project_id.is_none());
        assert_eq!(data.server_id, "alpha");
    }

    #[test]
    fn returns_error_when_neither_project_nor_server_found() {
        let args = SshArgs {
            id: Some("missing".to_string()),
            project: None,
            server: None,
            command: None,
            subcommand: None,
        };

        let error = run_with_loaders_and_executor(
            args,
            |_id| Err(homeboy::Error::project_not_found("missing")),
            |_id| Err(homeboy::Error::server_not_found("missing")),
            noop_executor,
        )
        .unwrap_err();

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn list_output_serializes_servers() {
        let output = SshOutput::List(SshListOutput {
            servers: vec![server("alpha"), server("beta")],
        });

        let value = serde_json::to_value(output).unwrap();
        assert_eq!(value["action"], "list");
        assert_eq!(value["servers"].as_array().unwrap().len(), 2);
    }
}
