use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::process::{Command, Stdio};

use homeboy_core::config::{ConfigManager, SlugIdentifiable};
use homeboy_core::context::{resolve_project_ssh, resolve_project_ssh_with_base_path};
use homeboy_core::plugin::{load_plugin, DatabaseCliConfig};
use homeboy_core::ssh::SshClient;
use homeboy_core::template::{render_map, TemplateVars};
use homeboy_core::token;

#[derive(Args)]
pub struct DbArgs {
    #[command(subcommand)]
    command: DbCommand,
}

#[derive(Subcommand)]
enum DbCommand {
    /// List database tables
    Tables {
        /// Project ID
        project_id: String,
        /// Optional subtarget followed by other args
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Show table structure
    Describe {
        /// Project ID
        project_id: String,
        /// Optional subtarget and table name
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Execute SELECT query
    Query {
        /// Project ID
        project_id: String,
        /// Optional subtarget and SQL query
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Delete a row from a table
    DeleteRow {
        /// Project ID
        project_id: String,
        /// Table name and row ID
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Confirm destructive operation
        #[arg(long)]
        confirm: bool,
    },
    /// Drop a database table
    DropTable {
        /// Project ID
        project_id: String,
        /// Table name
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Confirm destructive operation
        #[arg(long)]
        confirm: bool,
    },
    /// Open SSH tunnel to database
    Tunnel {
        /// Project ID
        project_id: String,
        /// Local port to bind
        #[arg(long)]
        local_port: Option<u16>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbOutput {
    pub command: String,
    pub project_id: String,
    pub base_path: Option<String>,
    pub domain: Option<String>,
    pub cli_path: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: i32,
    pub success: bool,
    pub tables: Option<Vec<String>>,
    pub table: Option<String>,
    pub sql: Option<String>,
    pub tunnel: Option<DbTunnelInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbTunnelInfo {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub database: String,
    pub user: String,
}

pub fn run(
    args: DbArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy_core::Result<(DbOutput, i32)> {
    match args.command {
        DbCommand::Tables { project_id, args } => tables(&project_id, &args),
        DbCommand::Describe { project_id, args } => describe(&project_id, &args),
        DbCommand::Query { project_id, args } => query(&project_id, &args),
        DbCommand::DeleteRow {
            project_id,
            args,
            confirm,
        } => delete_row(&project_id, &args, confirm),
        DbCommand::DropTable {
            project_id,
            args,
            confirm,
        } => drop_table(&project_id, &args, confirm),
        DbCommand::Tunnel {
            project_id,
            local_port,
        } => tunnel(&project_id, local_port),
    }
}

struct DbContext {
    project_id: String,
    client: SshClient,
    base_path: String,
    domain: String,
    cli_path: String,
    db_cli: DatabaseCliConfig,
}

fn build_context(
    project_id: &str,
    args: &[String],
) -> homeboy_core::Result<(DbContext, Vec<String>)> {
    let project = ConfigManager::load_project_record(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;

    let mut remaining_args = args.to_vec();
    let domain = if !project.config.sub_targets.is_empty() {
        if let Some(sub_id) = args.first() {
            if let Some(subtarget) = project.config.sub_targets.iter().find(|target| {
                target.slug_id().ok().as_deref() == Some(sub_id)
                    || token::identifier_eq(&target.name, sub_id)
            }) {
                remaining_args.remove(0);
                subtarget.domain.clone()
            } else {
                project.config.domain.clone()
            }
        } else {
            project.config.domain.clone()
        }
    } else {
        project.config.domain.clone()
    };

    let app_config = ConfigManager::load_app_config()?;

    // Find first plugin with database CLI config
    let db_cli = project
        .config
        .plugins
        .iter()
        .find_map(|plugin_id| {
            load_plugin(plugin_id)
                .and_then(|p| p.database)
                .and_then(|db| db.cli)
        })
        .ok_or_else(|| {
            homeboy_core::Error::config(
                "No plugin with database CLI configuration found".to_string(),
            )
        })?;

    let cli_path = project
        .config
        .plugins
        .iter()
        .find_map(|plugin_id| {
            load_plugin(plugin_id)
                .and_then(|p| p.cli)
                .and_then(|cli| cli.default_cli_path)
        })
        .unwrap_or_else(|| app_config.default_cli_path.clone());

    Ok((
        DbContext {
            project_id: project_id.to_string(),
            client: ctx.client,
            base_path,
            domain,
            cli_path,
            db_cli,
        },
        remaining_args,
    ))
}

fn parse_json_tables(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

fn tables(project_id: &str, args: &[String]) -> homeboy_core::Result<(DbOutput, i32)> {
    let (ctx, _) = build_context(project_id, args)?;

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    let command = render_map(&ctx.db_cli.tables_command, &vars);

    let output = ctx.client.execute(&command);
    let exit_code = output.exit_code;
    let success = output.success;
    let tables = if success {
        Some(parse_json_tables(&output.stdout))
    } else {
        None
    };

    Ok((
        DbOutput {
            command: "db.tables".to_string(),
            project_id: ctx.project_id,
            base_path: Some(ctx.base_path),
            domain: Some(ctx.domain),
            cli_path: Some(ctx.cli_path),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            exit_code,
            success,
            tables,
            table: None,
            sql: None,
            tunnel: None,
        },
        exit_code,
    ))
}

fn describe(project_id: &str, args: &[String]) -> homeboy_core::Result<(DbOutput, i32)> {
    let (ctx, remaining) = build_context(project_id, args)?;

    let table_name = remaining
        .first()
        .ok_or_else(|| homeboy_core::Error::config("Table name required".to_string()))?;

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::TABLE.to_string(), table_name.clone());
    let command = render_map(&ctx.db_cli.describe_command, &vars);

    let output = ctx.client.execute(&command);
    let exit_code = output.exit_code;

    Ok((
        DbOutput {
            command: "db.describe".to_string(),
            project_id: ctx.project_id,
            base_path: Some(ctx.base_path),
            domain: Some(ctx.domain),
            cli_path: Some(ctx.cli_path),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            exit_code,
            success: output.success,
            tables: None,
            table: Some(table_name.to_string()),
            sql: None,
            tunnel: None,
        },
        exit_code,
    ))
}

fn query(project_id: &str, args: &[String]) -> homeboy_core::Result<(DbOutput, i32)> {
    let (ctx, remaining) = build_context(project_id, args)?;

    let sql = remaining.join(" ");
    if sql.trim().is_empty() {
        return Err(homeboy_core::Error::config(
            "SQL query required".to_string(),
        ));
    }

    let forbidden_prefixes = [
        "INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "TRUNCATE", "CREATE", "REPLACE", "GRANT",
        "REVOKE",
    ];

    let upper_sql = sql.to_uppercase();
    let trimmed_sql = upper_sql.trim_start();
    if forbidden_prefixes
        .iter()
        .any(|keyword| trimmed_sql.starts_with(keyword))
    {
        return Err(homeboy_core::Error::config(
            "Write operations not allowed via 'db query'. Use the plugin CLI directly for writes."
                .to_string(),
        ));
    }

    let escaped_sql = sql.replace('\'', "''");

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::QUERY.to_string(), escaped_sql);
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = ctx.client.execute(&command);
    let exit_code = output.exit_code;

    Ok((
        DbOutput {
            command: "db.query".to_string(),
            project_id: ctx.project_id,
            base_path: Some(ctx.base_path),
            domain: Some(ctx.domain),
            cli_path: Some(ctx.cli_path),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            exit_code,
            success: output.success,
            tables: None,
            table: None,
            sql: Some(sql),
            tunnel: None,
        },
        exit_code,
    ))
}

fn delete_row(
    project_id: &str,
    args: &[String],
    confirm: bool,
) -> homeboy_core::Result<(DbOutput, i32)> {
    if !confirm {
        return Err(homeboy_core::Error::config(
            "Use --confirm to execute destructive operations".to_string(),
        ));
    }

    let (ctx, remaining) = build_context(project_id, args)?;

    if remaining.len() < 2 {
        return Err(homeboy_core::Error::config(
            "Table name and row ID required".to_string(),
        ));
    }

    let table_name = &remaining[0];
    let row_id = &remaining[1];

    row_id
        .parse::<i64>()
        .map_err(|_| homeboy_core::Error::config("Row ID must be numeric".to_string()))?;

    let delete_sql = format!("DELETE FROM {} WHERE ID = {} LIMIT 1", table_name, row_id);

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::QUERY.to_string(), delete_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = ctx.client.execute(&command);
    let exit_code = output.exit_code;

    Ok((
        DbOutput {
            command: "db.deleteRow".to_string(),
            project_id: ctx.project_id,
            base_path: Some(ctx.base_path),
            domain: Some(ctx.domain),
            cli_path: Some(ctx.cli_path),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            exit_code,
            success: output.success,
            tables: None,
            table: Some(table_name.to_string()),
            sql: Some(delete_sql),
            tunnel: None,
        },
        exit_code,
    ))
}

fn drop_table(
    project_id: &str,
    args: &[String],
    confirm: bool,
) -> homeboy_core::Result<(DbOutput, i32)> {
    if !confirm {
        return Err(homeboy_core::Error::config(
            "Use --confirm to execute destructive operations".to_string(),
        ));
    }

    let (ctx, remaining) = build_context(project_id, args)?;

    let table_name = remaining
        .first()
        .ok_or_else(|| homeboy_core::Error::config("Table name required".to_string()))?;

    let drop_sql = format!("DROP TABLE {}", table_name);

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::QUERY.to_string(), drop_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = ctx.client.execute(&command);
    let exit_code = output.exit_code;

    Ok((
        DbOutput {
            command: "db.dropTable".to_string(),
            project_id: ctx.project_id,
            base_path: Some(ctx.base_path),
            domain: Some(ctx.domain),
            cli_path: Some(ctx.cli_path),
            stdout: Some(output.stdout),
            stderr: Some(output.stderr),
            exit_code,
            success: output.success,
            tables: None,
            table: Some(table_name.to_string()),
            sql: Some(drop_sql),
            tunnel: None,
        },
        exit_code,
    ))
}

fn tunnel(project_id: &str, local_port: Option<u16>) -> homeboy_core::Result<(DbOutput, i32)> {
    let project = ConfigManager::load_project_record(project_id)?;
    let ctx = resolve_project_ssh(project_id)?;
    let server = ctx.server;
    let client = ctx.client;

    let app_config = ConfigManager::load_app_config()?;

    let remote_host = if project.config.database.host.is_empty() {
        app_config.default_database_host.clone()
    } else {
        project.config.database.host.clone()
    };

    let remote_port = project.config.database.port;
    let bind_port = local_port.unwrap_or(app_config.default_local_db_port);

    let tunnel_info = DbTunnelInfo {
        local_port: bind_port,
        remote_host: remote_host.clone(),
        remote_port,
        database: project.config.database.name.clone(),
        user: project.config.database.user.clone(),
    };

    let mut ssh_args = Vec::new();

    if let Some(identity_file) = &client.identity_file {
        ssh_args.push("-i".to_string());
        ssh_args.push(identity_file.clone());
    }

    if server.port != 22 {
        ssh_args.push("-p".to_string());
        ssh_args.push(server.port.to_string());
    }

    ssh_args.push("-N".to_string());
    ssh_args.push("-L".to_string());
    ssh_args.push(format!("{}:{}:{}", bind_port, remote_host, remote_port));
    ssh_args.push(format!("{}@{}", server.user, server.host));

    let status = Command::new("ssh")
        .args(&ssh_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let exit_code = match status {
        Ok(s) => s.code().unwrap_or(0),
        Err(e) => return Err(homeboy_core::Error::other(e.to_string())),
    };

    let success = exit_code == 0 || exit_code == 130;

    Ok((
        DbOutput {
            command: "db.tunnel".to_string(),
            project_id: project_id.to_string(),
            base_path: project.config.base_path.clone(),
            domain: Some(project.config.domain.clone()),
            cli_path: None,
            stdout: None,
            stderr: None,
            exit_code,
            success,
            tables: None,
            table: None,
            sql: None,
            tunnel: Some(tunnel_info),
        },
        exit_code,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_tables_handles_array() {
        let json = r#"["wp_posts", "wp_options", "wp_users"]"#;
        let tables = parse_json_tables(json);
        assert_eq!(tables, vec!["wp_posts", "wp_options", "wp_users"]);
    }

    #[test]
    fn parse_json_tables_returns_empty_on_invalid() {
        let invalid = "not json";
        let tables = parse_json_tables(invalid);
        assert!(tables.is_empty());
    }
}
