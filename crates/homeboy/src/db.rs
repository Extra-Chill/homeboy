use serde::Serialize;
use std::collections::HashMap;
use std::process::{Command, Stdio};

use crate::context::{resolve_project_ssh, resolve_project_ssh_with_base_path};
use crate::module::{load_all_modules, DatabaseCliConfig};
use crate::project::{self, ProjectRecord};
use crate::ssh::SshClient;
use crate::template::{render_map, TemplateVars};
use crate::token;
use crate::{Error, Result};

const DEFAULT_DATABASE_HOST: &str = "127.0.0.1";
const DEFAULT_LOCAL_DB_PORT: u16 = 33306;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DbResult {
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
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DbTunnelInfo {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub database: String,
    pub user: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DbTunnelResult {
    pub project_id: String,
    pub base_path: Option<String>,
    pub domain: Option<String>,
    pub exit_code: i32,
    pub success: bool,
    pub tunnel: DbTunnelInfo,
}

struct DbContext {
    project_id: String,
    client: SshClient,
    base_path: String,
    domain: String,
    cli_path: String,
    db_cli: DatabaseCliConfig,
}

fn build_context(project_id: &str, subtarget: Option<&str>) -> Result<DbContext> {
    let project = project::load_record(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;

    let domain = resolve_domain(&project, subtarget);

    let modules = load_all_modules();

    let db_cli = modules
        .iter()
        .find_map(|m| m.database.as_ref().and_then(|db| db.cli.clone()))
        .ok_or_else(|| {
            Error::config("No module with database CLI configuration found".to_string())
        })?;

    let cli_path = modules
        .iter()
        .find_map(|m| m.cli.as_ref().and_then(|cli| cli.default_cli_path.clone()))
        .unwrap_or_default();

    Ok(DbContext {
        project_id: project_id.to_string(),
        client: ctx.client,
        base_path,
        domain,
        cli_path,
        db_cli,
    })
}

fn resolve_domain(project: &ProjectRecord, subtarget: Option<&str>) -> String {
    if project.config.sub_targets.is_empty() {
        return project.config.domain.clone();
    }

    let Some(sub_id) = subtarget else {
        return project.config.domain.clone();
    };

    if let Some(target) = project.config.sub_targets.iter().find(|t| {
        project::slugify_id(&t.name).ok().as_deref() == Some(sub_id)
            || token::identifier_eq(&t.name, sub_id)
    }) {
        return target.domain.clone();
    }

    project.config.domain.clone()
}

fn parse_json_tables(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

pub fn list_tables(project_id: &str, subtarget: Option<&str>) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    let command = render_map(&ctx.db_cli.tables_command, &vars);

    let output = ctx.client.execute(&command);
    let tables = if output.success {
        Some(parse_json_tables(&output.stdout))
    } else {
        None
    };

    Ok(DbResult {
        project_id: ctx.project_id,
        base_path: Some(ctx.base_path),
        domain: Some(ctx.domain),
        cli_path: Some(ctx.cli_path),
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: output.exit_code,
        success: output.success,
        tables,
        table: None,
        sql: None,
    })
}

pub fn describe_table(project_id: &str, table: &str, subtarget: Option<&str>) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::TABLE.to_string(), table.to_string());
    let command = render_map(&ctx.db_cli.describe_command, &vars);

    let output = ctx.client.execute(&command);

    Ok(DbResult {
        project_id: ctx.project_id,
        base_path: Some(ctx.base_path),
        domain: Some(ctx.domain),
        cli_path: Some(ctx.cli_path),
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: output.exit_code,
        success: output.success,
        tables: None,
        table: Some(table.to_string()),
        sql: None,
    })
}

pub fn query(project_id: &str, sql: &str, subtarget: Option<&str>) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    if sql.trim().is_empty() {
        return Err(Error::config("SQL query required".to_string()));
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
        return Err(Error::config(
            "Write operations not allowed via query. Use the module CLI directly for writes."
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

    Ok(DbResult {
        project_id: ctx.project_id,
        base_path: Some(ctx.base_path),
        domain: Some(ctx.domain),
        cli_path: Some(ctx.cli_path),
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: output.exit_code,
        success: output.success,
        tables: None,
        table: None,
        sql: Some(sql.to_string()),
    })
}

pub fn delete_row(
    project_id: &str,
    table: &str,
    row_id: i64,
    subtarget: Option<&str>,
) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    let delete_sql = format!("DELETE FROM {} WHERE ID = {} LIMIT 1", table, row_id);

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::QUERY.to_string(), delete_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = ctx.client.execute(&command);

    Ok(DbResult {
        project_id: ctx.project_id,
        base_path: Some(ctx.base_path),
        domain: Some(ctx.domain),
        cli_path: Some(ctx.cli_path),
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: output.exit_code,
        success: output.success,
        tables: None,
        table: Some(table.to_string()),
        sql: Some(delete_sql),
    })
}

pub fn drop_table(project_id: &str, table: &str, subtarget: Option<&str>) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    let drop_sql = format!("DROP TABLE {}", table);

    let mut vars = HashMap::new();
    vars.insert(TemplateVars::SITE_PATH.to_string(), ctx.base_path.clone());
    vars.insert(TemplateVars::CLI_PATH.to_string(), ctx.cli_path.clone());
    vars.insert(TemplateVars::QUERY.to_string(), drop_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = ctx.client.execute(&command);

    Ok(DbResult {
        project_id: ctx.project_id,
        base_path: Some(ctx.base_path),
        domain: Some(ctx.domain),
        cli_path: Some(ctx.cli_path),
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: output.exit_code,
        success: output.success,
        tables: None,
        table: Some(table.to_string()),
        sql: Some(drop_sql),
    })
}

pub fn create_tunnel(project_id: &str, local_port: Option<u16>) -> Result<DbTunnelResult> {
    let project = project::load_record(project_id)?;
    let ctx = resolve_project_ssh(project_id)?;
    let server = ctx.server;
    let client = ctx.client;

    let remote_host = if project.config.database.host.is_empty() {
        DEFAULT_DATABASE_HOST.to_string()
    } else {
        project.config.database.host.clone()
    };

    let remote_port = project.config.database.port;
    let bind_port = local_port.unwrap_or(DEFAULT_LOCAL_DB_PORT);

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
        Err(e) => return Err(Error::other(e.to_string())),
    };

    let success = exit_code == 0 || exit_code == 130;

    Ok(DbTunnelResult {
        project_id: project_id.to_string(),
        base_path: project.config.base_path.clone(),
        domain: Some(project.config.domain.clone()),
        exit_code,
        success,
        tunnel: tunnel_info,
    })
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
