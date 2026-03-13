//! Database query and table operations.
//!
//! Provides list_tables, describe_table, query, search, delete_row, and drop_table
//! operations that execute through extension-defined CLI commands.

use serde::Serialize;
use std::collections::HashMap;

use crate::context::require_project_base_path;
use crate::engine::executor::execute_for_project;
use crate::engine::text;
use crate::engine::template::{render_map, TemplateVars};
use crate::extension::{load_all_extensions, DatabaseCliConfig};
use crate::project::{self, Project};
use crate::{Error, Result};

#[derive(Serialize, Clone)]
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

struct DbContext {
    project: Project,
    base_path: String,
    domain: String,
    cli_path: String,
    db_cli: DatabaseCliConfig,
}

impl DbContext {
    /// Build base template variables for database commands.
    fn base_template_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::with_capacity(8);
        vars.insert(TemplateVars::SITE_PATH.to_string(), self.base_path.clone());
        vars.insert(TemplateVars::CLI_PATH.to_string(), self.cli_path.clone());
        vars.insert(
            TemplateVars::DB_HOST.to_string(),
            self.project.database.host.clone(),
        );
        vars.insert(
            TemplateVars::DB_PORT.to_string(),
            self.project.database.port.to_string(),
        );
        vars.insert(
            TemplateVars::DB_NAME.to_string(),
            self.project.database.name.clone(),
        );
        vars.insert(
            TemplateVars::DB_USER.to_string(),
            self.project.database.user.clone(),
        );
        vars.insert(TemplateVars::DB_PASSWORD.to_string(), String::new());
        vars
    }
}

fn build_context(project_id: &str, subtarget: Option<&str>) -> Result<DbContext> {
    let project = project::load(project_id)?;
    let base_path = require_project_base_path(project_id, &project)?;

    let domain = resolve_domain(&project, subtarget, project_id)?;

    let extensions = load_all_extensions().unwrap_or_default();

    let db_cli = extensions
        .iter()
        .find_map(|m| m.database().and_then(|db| db.cli.clone()))
        .ok_or_else(|| {
            Error::config("No extension with database CLI configuration found".to_string())
        })?;

    let cli_path = extensions
        .iter()
        .find_map(|m| m.cli.as_ref().and_then(|cli| cli.default_cli_path.clone()))
        .unwrap_or_default();

    Ok(DbContext {
        project,
        base_path,
        domain,
        cli_path,
        db_cli,
    })
}

fn resolve_domain(project: &Project, subtarget: Option<&str>, project_id: &str) -> Result<String> {
    let require_domain = || {
        Error::validation_invalid_argument(
            "domain",
            "This operation requires a domain to be configured on the project",
            Some(project_id.to_string()),
            None,
        )
    };

    if project.sub_targets.is_empty() {
        return project.domain.clone().ok_or_else(require_domain);
    }

    let Some(sub_id) = subtarget else {
        let subtarget_list = project
            .sub_targets
            .iter()
            .map(|t| {
                let slug = project::slugify_id(&t.name).unwrap_or_else(|_| t.name.clone());
                format!("- {} (use: {})", t.name, slug)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::validation_invalid_argument(
            "subtarget",
            format!(
                "This project has subtargets configured. You must specify which subtarget to use.\n\nAvailable subtargets for project '{}':\n{}",
                project_id, subtarget_list
            ),
            Some(project_id.to_string()),
            None,
        ));
    };

    if let Some(target) = project.sub_targets.iter().find(|t| {
        project::slugify_id(&t.name).ok().as_deref() == Some(sub_id)
            || text::identifier_eq(&t.name, sub_id)
    }) {
        return Ok(target.domain.clone());
    }

    let subtarget_list = project
        .sub_targets
        .iter()
        .map(|t| {
            let slug = project::slugify_id(&t.name).unwrap_or_else(|_| t.name.clone());
            format!("- {} (use: {})", t.name, slug)
        })
        .collect::<Vec<_>>()
        .join("\n");
    Err(Error::validation_invalid_argument(
        "subtarget",
        format!(
            "Subtarget '{}' not found. Available subtargets for project '{}':\n{}",
            sub_id, project_id, subtarget_list
        ),
        Some(project_id.to_string()),
        None,
    ))
}

fn parse_json_tables(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

pub fn list_tables(project_id: &str, subtarget: Option<&str>) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    let vars = ctx.base_template_vars();
    let command = render_map(&ctx.db_cli.tables_command, &vars);

    let output = execute_for_project(&ctx.project, &command)?;
    let tables = if output.success {
        Some(parse_json_tables(&output.stdout))
    } else {
        None
    };

    Ok(DbResult {
        project_id: ctx.project.id.clone(),
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

pub fn describe_table(
    project_id: &str,
    table: Option<&str>,
    subtarget: Option<&str>,
) -> Result<DbResult> {
    let table = table.ok_or_else(|| Error::config("Table name required".to_string()))?;
    let ctx = build_context(project_id, subtarget)?;

    let mut vars = ctx.base_template_vars();
    vars.insert(TemplateVars::TABLE.to_string(), table.to_string());
    let command = render_map(&ctx.db_cli.describe_command, &vars);

    let output = execute_for_project(&ctx.project, &command)?;

    Ok(DbResult {
        project_id: ctx.project.id.clone(),
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
            "Write operations not allowed via query. Use the extension CLI directly for writes."
                .to_string(),
        ));
    }

    let escaped_sql = sql.replace('\'', "''");

    let mut vars = ctx.base_template_vars();
    vars.insert(TemplateVars::QUERY.to_string(), escaped_sql);
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = execute_for_project(&ctx.project, &command)?;

    Ok(DbResult {
        project_id: ctx.project.id.clone(),
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

const DEFAULT_SEARCH_LIMIT: u32 = 100;

pub fn search(
    project_id: &str,
    table: &str,
    column: &str,
    pattern: &str,
    exact: bool,
    limit: Option<u32>,
    subtarget: Option<&str>,
) -> Result<DbResult> {
    let ctx = build_context(project_id, subtarget)?;

    if table.trim().is_empty() {
        return Err(Error::config("Table name required".to_string()));
    }
    if column.trim().is_empty() {
        return Err(Error::config("Column name required".to_string()));
    }
    if pattern.trim().is_empty() {
        return Err(Error::config("Search pattern required".to_string()));
    }

    let escaped_pattern = pattern.replace('\'', "''");
    let row_limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);

    let search_sql = if exact {
        format!(
            "SELECT * FROM {} WHERE {} = '{}' LIMIT {}",
            table, column, escaped_pattern, row_limit
        )
    } else {
        format!(
            "SELECT * FROM {} WHERE {} LIKE '%{}%' LIMIT {}",
            table, column, escaped_pattern, row_limit
        )
    };

    let mut vars = ctx.base_template_vars();
    vars.insert(TemplateVars::QUERY.to_string(), search_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = execute_for_project(&ctx.project, &command)?;

    Ok(DbResult {
        project_id: ctx.project.id.clone(),
        base_path: Some(ctx.base_path),
        domain: Some(ctx.domain),
        cli_path: Some(ctx.cli_path),
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: output.exit_code,
        success: output.success,
        tables: None,
        table: Some(table.to_string()),
        sql: Some(search_sql),
    })
}

pub fn delete_row(
    project_id: &str,
    table: Option<&str>,
    row_id: Option<&str>,
    subtarget: Option<&str>,
) -> Result<DbResult> {
    let table = table.ok_or_else(|| Error::config("Table name required".to_string()))?;
    let row_id: i64 = row_id
        .ok_or_else(|| Error::config("Row ID required".to_string()))?
        .parse()
        .map_err(|_| Error::config("Row ID must be numeric".to_string()))?;
    let ctx = build_context(project_id, subtarget)?;

    let delete_sql = format!("DELETE FROM {} WHERE ID = {} LIMIT 1", table, row_id);

    let mut vars = ctx.base_template_vars();
    vars.insert(TemplateVars::QUERY.to_string(), delete_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = execute_for_project(&ctx.project, &command)?;

    Ok(DbResult {
        project_id: ctx.project.id.clone(),
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

pub fn drop_table(
    project_id: &str,
    table: Option<&str>,
    subtarget: Option<&str>,
) -> Result<DbResult> {
    let table = table.ok_or_else(|| Error::config("Table name required".to_string()))?;
    let ctx = build_context(project_id, subtarget)?;

    let drop_sql = format!("DROP TABLE {}", table);

    let mut vars = ctx.base_template_vars();
    vars.insert(TemplateVars::QUERY.to_string(), drop_sql.clone());
    vars.insert(TemplateVars::FORMAT.to_string(), "json".to_string());
    vars.insert(TemplateVars::DOMAIN.to_string(), ctx.domain.clone());
    let command = render_map(&ctx.db_cli.query_command, &vars);

    let output = execute_for_project(&ctx.project, &command)?;

    Ok(DbResult {
        project_id: ctx.project.id.clone(),
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
