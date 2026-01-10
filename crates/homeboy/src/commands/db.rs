use clap::{Args, Subcommand};
use serde::Serialize;
use std::process::{Command, Stdio};
use homeboy_core::config::{ConfigManager, ProjectTypeManager};
use homeboy_core::ssh::SshClient;
use homeboy_core::output::{print_success, print_error};

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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show table structure
    Describe {
        /// Project ID
        project_id: String,
        /// Optional subtarget and table name
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Execute SELECT query
    Query {
        /// Project ID
        project_id: String,
        /// Optional subtarget and SQL query
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Open SSH tunnel to database
    Tunnel {
        /// Project ID
        project_id: String,
        /// Local port to bind
        #[arg(long)]
        local_port: Option<u16>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: DbArgs) {
    match args.command {
        DbCommand::Tables { project_id, args, json } => tables(&project_id, &args, json),
        DbCommand::Describe { project_id, args, json } => describe(&project_id, &args, json),
        DbCommand::Query { project_id, args, json } => query(&project_id, &args, json),
        DbCommand::DeleteRow { project_id, args, confirm, json } => {
            delete_row(&project_id, &args, confirm, json)
        }
        DbCommand::DropTable { project_id, args, confirm, json } => {
            drop_table(&project_id, &args, confirm, json)
        }
        DbCommand::Tunnel { project_id, local_port, json } => tunnel(&project_id, local_port, json),
    }
}

struct DbContext {
    project_id: String,
    server_id: String,
    client: SshClient,
    base_path: String,
    domain: String,
    cli_path: String,
}

fn build_context(project_id: &str, args: &[String], json: bool) -> Option<(DbContext, Vec<String>)> {
    let project = match ConfigManager::load_project(project_id) {
        Ok(p) => p,
        Err(e) => {
            if json {
                print_error(e.code(), &e.to_string());
            } else {
                eprintln!("Error: {}", e);
            }
            return None;
        }
    };

    let server_id = match &project.server_id {
        Some(id) => id.clone(),
        None => {
            let msg = format!("Server not configured for project '{}'", project_id);
            if json { print_error("SERVER_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return None;
        }
    };

    let server = match ConfigManager::load_server(&server_id) {
        Ok(s) => s,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return None;
        }
    };

    let base_path = match &project.base_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            let msg = format!("Base path not configured for project '{}'", project_id);
            if json { print_error("BASE_PATH_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return None;
        }
    };

    let client = match SshClient::from_server(&server, &server_id) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error("SSH_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return None;
        }
    };

    // Resolve subtarget
    let mut remaining_args = args.to_vec();
    let domain = if !project.sub_targets.is_empty() && !args.is_empty() {
        let potential = args[0].to_lowercase();
        if let Some(st) = project.sub_targets.iter().find(|t| {
            t.id.to_lowercase() == potential || t.name.to_lowercase() == potential
        }) {
            remaining_args.remove(0);
            st.domain.clone()
        } else {
            project.domain.clone()
        }
    } else {
        project.domain.clone()
    };

    let type_def = ProjectTypeManager::resolve(&project.project_type);
    let cli_path = type_def
        .cli
        .as_ref()
        .and_then(|c| c.default_cli_path.clone())
        .unwrap_or_else(|| "wp".to_string());

    Some((
        DbContext {
            project_id: project_id.to_string(),
            server_id,
            client,
            base_path,
            domain,
            cli_path,
        },
        remaining_args,
    ))
}

fn parse_wp_db_tables_csv(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wp_db_tables_csv_trims_and_filters() {
        let csv = "wp_posts, wp_options,,\nwp_users\n";
        let tables = parse_wp_db_tables_csv(csv);
        assert_eq!(tables, vec!["wp_posts", "wp_options", "wp_users"]);
    }
}

fn tables(project_id: &str, args: &[String], json: bool) {
    let (ctx, _) = match build_context(project_id, args, json) {
        Some(c) => c,
        None => return,
    };

    if json {
        let command = format!(
            "cd '{}' && {} db tables --format=csv",
            ctx.base_path, ctx.cli_path
        );

        let output = ctx.client.execute(&command);
        if !output.success {
            print_error("WP_CLI_ERROR", &output.stderr);
            std::process::exit(output.exit_code);
        }

        let tables = parse_wp_db_tables_csv(&output.stdout);
        print_success(tables);
        return;
    }

    let command = format!(
        "cd '{}' && {} db tables --format=list",
        ctx.base_path, ctx.cli_path
    );

    let output = ctx.client.execute(&command);
    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    if !output.success {
        std::process::exit(output.exit_code);
    }
}

fn describe(project_id: &str, args: &[String], json: bool) {
    let (ctx, remaining) = match build_context(project_id, args, json) {
        Some(c) => c,
        None => return,
    };

    let table_name = match remaining.first() {
        Some(t) => t,
        None => {
            if json {
                print_error("MISSING_TABLE", "Table name required");
            } else {
                eprintln!("Error: Table name required");
                eprintln!("Usage: homeboy db <project> describe <table>");
            }
            return;
        }
    };

    let command = format!(
        "cd '{}' && {} db columns {} --format={}",
        ctx.base_path,
        ctx.cli_path,
        table_name,
        if json { "json" } else { "table" }
    );

    let output = ctx.client.execute(&command);
    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    if !output.success {
        std::process::exit(output.exit_code);
    }
}

fn query(project_id: &str, args: &[String], json: bool) {
    let (ctx, remaining) = match build_context(project_id, args, json) {
        Some(c) => c,
        None => return,
    };

    let sql = remaining.join(" ");
    if sql.is_empty() {
        if json {
            print_error("MISSING_QUERY", "SQL query required");
        } else {
            eprintln!("Error: SQL query required");
            eprintln!("Usage: homeboy db <project> query \"SELECT * FROM table\"");
        }
        return;
    }

    // Validate read-only
    let forbidden = ["INSERT", "UPDATE", "DELETE", "DROP", "ALTER", "TRUNCATE", "CREATE", "REPLACE", "GRANT", "REVOKE"];
    let upper_sql = sql.to_uppercase();
    for keyword in forbidden {
        if upper_sql.trim_start().starts_with(keyword) {
            if json {
                print_error("WRITE_NOT_ALLOWED", "Write operations not allowed via 'db query'. Use 'homeboy wp <project> db query' for writes.");
            } else {
                eprintln!("Error: Write operations not allowed via 'db query'.");
                eprintln!("Use 'homeboy wp <project> db query' for write operations.");
            }
            return;
        }
    }

    let escaped_sql = sql.replace('"', "\\\"");
    let command = format!(
        "cd '{}' && {} db query \"{}\" --format={} --url='{}'",
        ctx.base_path,
        ctx.cli_path,
        escaped_sql,
        if json { "json" } else { "table" },
        ctx.domain
    );

    let output = ctx.client.execute(&command);
    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }

    if !output.success {
        std::process::exit(output.exit_code);
    }
}

fn delete_row(project_id: &str, args: &[String], confirm: bool, json: bool) {
    if !confirm {
        if json {
            print_error("CONFIRM_REQUIRED", "Use --confirm to execute destructive operations");
        } else {
            eprintln!("Error: Use --confirm flag to execute destructive operations");
        }
        return;
    }

    let (ctx, remaining) = match build_context(project_id, args, json) {
        Some(c) => c,
        None => return,
    };

    if remaining.len() < 2 {
        if json {
            print_error("MISSING_ARGS", "Table name and row ID required");
        } else {
            eprintln!("Error: Table name and row ID required");
            eprintln!("Usage: homeboy db <project> delete-row <table> <id> --confirm");
        }
        return;
    }

    let table_name = &remaining[0];
    let row_id = &remaining[1];

    if row_id.parse::<i64>().is_err() {
        if json {
            print_error("INVALID_ID", "Row ID must be numeric");
        } else {
            eprintln!("Error: Row ID must be numeric");
        }
        return;
    }

    let delete_sql = format!("DELETE FROM {} WHERE ID = {} LIMIT 1", table_name, row_id);
    let command = format!(
        "cd '{}' && {} db query \"{}\" --url='{}'",
        ctx.base_path, ctx.cli_path, delete_sql, ctx.domain
    );

    let output = ctx.client.execute(&command);

    if output.success {
        if json {
            #[derive(Serialize)]
            struct DeleteResult {
                table: String,
                #[serde(rename = "rowId")]
                row_id: String,
            }
            print_success(DeleteResult {
                table: table_name.clone(),
                row_id: row_id.clone(),
            });
        } else {
            println!("Deleted row {} from {}", row_id, table_name);
        }
    } else {
        if json {
            print_error("DELETE_FAILED", &output.stderr);
        } else {
            eprintln!("Error: {}", output.stderr);
        }
        std::process::exit(output.exit_code);
    }
}

fn drop_table(project_id: &str, args: &[String], confirm: bool, json: bool) {
    if !confirm {
        if json {
            print_error("CONFIRM_REQUIRED", "Use --confirm to execute destructive operations");
        } else {
            eprintln!("Error: Use --confirm flag to execute destructive operations");
        }
        return;
    }

    let (ctx, remaining) = match build_context(project_id, args, json) {
        Some(c) => c,
        None => return,
    };

    let table_name = match remaining.first() {
        Some(t) => t,
        None => {
            if json {
                print_error("MISSING_TABLE", "Table name required");
            } else {
                eprintln!("Error: Table name required");
                eprintln!("Usage: homeboy db <project> drop-table <table> --confirm");
            }
            return;
        }
    };

    let drop_sql = format!("DROP TABLE {}", table_name);
    let command = format!(
        "cd '{}' && {} db query \"{}\" --url='{}'",
        ctx.base_path, ctx.cli_path, drop_sql, ctx.domain
    );

    let output = ctx.client.execute(&command);

    if output.success {
        if json {
            #[derive(Serialize)]
            struct DropResult {
                table: String,
            }
            print_success(DropResult {
                table: table_name.clone(),
            });
        } else {
            println!("Dropped table: {}", table_name);
        }
    } else {
        if json {
            print_error("DROP_FAILED", &output.stderr);
        } else {
            eprintln!("Error: {}", output.stderr);
        }
        std::process::exit(output.exit_code);
    }
}

fn tunnel(project_id: &str, local_port: Option<u16>, json: bool) {
    let project = match ConfigManager::load_project(project_id) {
        Ok(p) => p,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let server_id = match &project.server_id {
        Some(id) => id,
        None => {
            let msg = format!("Server not configured for project '{}'", project_id);
            if json { print_error("SERVER_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    let server = match ConfigManager::load_server(server_id) {
        Ok(s) => s,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let client = match homeboy_core::ssh::SshClient::from_server(&server, server_id) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error("SSH_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let remote_host = if project.database.host.is_empty() {
        "127.0.0.1"
    } else {
        &project.database.host
    };
    let remote_port = project.database.port;
    let bind_port = local_port.unwrap_or(33306);

    if json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct TunnelInfo {
            local_port: u16,
            remote_host: String,
            remote_port: u16,
            database: String,
            user: String,
        }
        print_success(TunnelInfo {
            local_port: bind_port,
            remote_host: remote_host.to_string(),
            remote_port,
            database: project.database.name.clone(),
            user: project.database.user.clone(),
        });
    } else {
        println!("Opening SSH tunnel to {}...", project.database.name);
        println!("Local:  127.0.0.1:{}", bind_port);
        println!("Remote: {}:{}", remote_host, remote_port);
        println!();
        println!(
            "Connect with: mysql -h 127.0.0.1 -P {} -u {} -p {}",
            bind_port, project.database.user, project.database.name
        );
        println!();
        println!("Press Ctrl+C to close the tunnel.");
    }

    let mut args = Vec::new();

    if let Some(identity_file) = &client.identity_file {
        args.push("-i".to_string());
        args.push(identity_file.clone());
    }

    if server.port != 22 {
        args.push("-p".to_string());
        args.push(server.port.to_string());
    }

    args.push("-N".to_string());
    args.push("-L".to_string());
    args.push(format!("{}:{}:{}", bind_port, remote_host, remote_port));
    args.push(format!("{}@{}", server.user, server.host));

    let status = Command::new("ssh")
        .args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    if let Ok(s) = status {
        let code = s.code().unwrap_or(0);
        if code != 0 && code != 130 {
            std::process::exit(code);
        }
    }
}
