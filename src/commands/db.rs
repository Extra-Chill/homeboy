use clap::{Args, Subcommand};
use serde::{Serialize, Serializer};

use homeboy::core::db::{self, DbResult, DbTunnelResult};
use homeboy::core::engine::text;
use homeboy::core::observation::store::{self, ObservationDbStatus};
use homeboy::core::project;

use super::CmdResult;

#[derive(Args)]
pub struct DbArgs {
    #[command(subcommand)]
    command: DbCommand,
}

#[derive(Subcommand)]
enum DbCommand {
    /// Show local Homeboy observation-store status
    Status,
    /// List database tables
    Tables {
        /// Project ID
        project_id: String,
        /// Optional subtarget
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
    /// Search table by column value
    Search {
        /// Project ID
        project_id: String,
        /// Table name
        table: String,
        /// Column to search
        #[arg(long)]
        column: String,
        /// Search pattern
        #[arg(long)]
        pattern: String,
        /// Use exact match instead of LIKE
        #[arg(long, default_value_t = false)]
        exact: bool,
        /// Maximum rows to return
        #[arg(long)]
        limit: Option<u32>,
        /// Optional subtarget
        #[arg(long)]
        subtarget: Option<String>,
    },
    /// Delete a row from a table
    DeleteRow {
        /// Project ID
        project_id: String,
        /// Table name and row ID
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Drop a database table
    DropTable {
        /// Project ID
        project_id: String,
        /// Table name
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
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
pub struct DbOutput {
    pub command: String,
    #[serde(flatten)]
    pub result: DbResultVariant,
}

pub enum DbResultVariant {
    Status(ObservationDbStatus),
    Query(DbResult),
    Tunnel(DbTunnelResult),
}

#[derive(Serialize)]
struct TaggedDbResult<'a, T: Serialize> {
    variant: &'static str,
    #[serde(flatten)]
    result: &'a T,
}

impl Serialize for DbResultVariant {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            DbResultVariant::Status(result) => TaggedDbResult {
                variant: "status",
                result,
            }
            .serialize(serializer),
            DbResultVariant::Query(result) => TaggedDbResult {
                variant: "query",
                result,
            }
            .serialize(serializer),
            DbResultVariant::Tunnel(result) => TaggedDbResult {
                variant: "tunnel",
                result,
            }
            .serialize(serializer),
        }
    }
}

pub fn run(args: DbArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DbOutput> {
    match args.command {
        DbCommand::Status => status(),
        DbCommand::Tables { project_id, args } => tables(&project_id, &args),
        DbCommand::Describe { project_id, args } => describe(&project_id, &args),
        DbCommand::Query { project_id, args } => query(&project_id, &args),
        DbCommand::Search {
            project_id,
            table,
            column,
            pattern,
            exact,
            limit,
            subtarget,
        } => search(
            &project_id,
            &table,
            &column,
            &pattern,
            exact,
            limit,
            subtarget.as_deref(),
        ),
        DbCommand::DeleteRow { project_id, args } => delete_row(&project_id, &args),
        DbCommand::DropTable { project_id, args } => drop_table(&project_id, &args),
        DbCommand::Tunnel {
            project_id,
            local_port,
        } => tunnel(&project_id, local_port),
    }
}

fn status() -> CmdResult<DbOutput> {
    Ok((
        DbOutput {
            command: "db.status".to_string(),
            result: DbResultVariant::Status(store::status()?),
        },
        0,
    ))
}

fn parse_subtarget(
    project_id: &str,
    args: &[String],
) -> homeboy::core::Result<(Option<String>, Vec<String>)> {
    let project = project::load(project_id)?;

    if project.sub_targets.is_empty() {
        return Ok((None, args.to_vec()));
    }

    if let Some(sub_id) = args.first() {
        if project.sub_targets.iter().any(|target| {
            project::slugify_id(&target.name).ok().as_deref() == Some(sub_id)
                || text::identifier_eq(&target.name, sub_id)
        }) {
            return Ok((Some(sub_id.clone()), args[1..].to_vec()));
        }
    }

    Ok((None, args.to_vec()))
}

fn tables(project_id: &str, args: &[String]) -> CmdResult<DbOutput> {
    let (subtarget, _) = parse_subtarget(project_id, args)?;
    let result = db::list_tables(project_id, subtarget.as_deref())?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.tables".to_string(),
            result: DbResultVariant::Query(result),
        },
        exit_code,
    ))
}

fn describe(project_id: &str, args: &[String]) -> CmdResult<DbOutput> {
    let (subtarget, remaining) = parse_subtarget(project_id, args)?;

    // Core validates table_name
    let table_name = remaining.first().map(|s| s.as_str());
    let result = db::describe_table(project_id, table_name, subtarget.as_deref())?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.describe".to_string(),
            result: DbResultVariant::Query(result),
        },
        exit_code,
    ))
}

fn query(project_id: &str, args: &[String]) -> CmdResult<DbOutput> {
    let (subtarget, remaining) = parse_subtarget(project_id, args)?;
    let sql = remaining.join(" ");

    let result = db::query(project_id, &sql, subtarget.as_deref())?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.query".to_string(),
            result: DbResultVariant::Query(result),
        },
        exit_code,
    ))
}

fn search(
    project_id: &str,
    table: &str,
    column: &str,
    pattern: &str,
    exact: bool,
    limit: Option<u32>,
    subtarget: Option<&str>,
) -> CmdResult<DbOutput> {
    let result = db::search(project_id, table, column, pattern, exact, limit, subtarget)?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.search".to_string(),
            result: DbResultVariant::Query(result),
        },
        exit_code,
    ))
}

fn delete_row(project_id: &str, args: &[String]) -> CmdResult<DbOutput> {
    let (subtarget, remaining) = parse_subtarget(project_id, args)?;

    // Core validates table_name and row_id
    let table_name = remaining.first().map(|s| s.as_str());
    let row_id = remaining.get(1).map(|s| s.as_str());
    let result = db::delete_row(project_id, table_name, row_id, subtarget.as_deref())?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.deleteRow".to_string(),
            result: DbResultVariant::Query(result),
        },
        exit_code,
    ))
}

fn drop_table(project_id: &str, args: &[String]) -> CmdResult<DbOutput> {
    let (subtarget, remaining) = parse_subtarget(project_id, args)?;

    // Core validates table_name
    let table_name = remaining.first().map(|s| s.as_str());
    let result = db::drop_table(project_id, table_name, subtarget.as_deref())?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.dropTable".to_string(),
            result: DbResultVariant::Query(result),
        },
        exit_code,
    ))
}

fn tunnel(project_id: &str, local_port: Option<u16>) -> CmdResult<DbOutput> {
    let result = db::create_tunnel(project_id, local_port)?;
    let exit_code = result.exit_code;

    Ok((
        DbOutput {
            command: "db.tunnel".to_string(),
            result: DbResultVariant::Tunnel(result),
        },
        exit_code,
    ))
}
