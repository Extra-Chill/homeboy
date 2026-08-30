#!/usr/bin/env bash

# Generic disposable local MySQL/MariaDB helper for extension runners.
#
# The caller owns framework-specific semantics by choosing which environment
# variable names receive the connection values. Homeboy only owns a local,
# socket-only database lifecycle and cleanup.

HOMEBOY_DISPOSABLE_LOCAL_DB_DIR="${HOMEBOY_DISPOSABLE_LOCAL_DB_DIR:-}"
HOMEBOY_DISPOSABLE_LOCAL_DB_PID="${HOMEBOY_DISPOSABLE_LOCAL_DB_PID:-}"
HOMEBOY_DISPOSABLE_LOCAL_DB_SOCKET="${HOMEBOY_DISPOSABLE_LOCAL_DB_SOCKET:-}"
HOMEBOY_DISPOSABLE_LOCAL_DB_CLIENT="${HOMEBOY_DISPOSABLE_LOCAL_DB_CLIENT:-}"
HOMEBOY_DISPOSABLE_LOCAL_DB_STOP_REGISTERED="${HOMEBOY_DISPOSABLE_LOCAL_DB_STOP_REGISTERED:-0}"

homeboy_disposable_local_db_usage() {
    cat <<'EOF'
Usage: homeboy_disposable_local_db_start [options]

Starts an isolated local MySQL/MariaDB instance on a Unix socket, creates one
database/user, exports requested env vars, and registers EXIT cleanup.

Options:
  --database NAME          Database name to create (default: homeboy_test)
  --user NAME              Database user to create (default: homeboy)
  --password VALUE         Database password (default: generated)
  --tmp-dir PATH           Parent temp directory (default: mktemp under TMPDIR)
  --env-host NAME          Export NAME=localhost
  --env-port NAME          Export NAME=0
  --env-socket NAME        Export NAME=<socket path>
  --env-database NAME      Export NAME=<database>
  --env-user NAME          Export NAME=<user>
  --env-password NAME      Export NAME=<password>
EOF
}

homeboy_disposable_local_db_find_command() {
    local candidate
    for candidate in "$@"; do
        if command -v "$candidate" >/dev/null 2>&1; then
            command -v "$candidate"
            return 0
        fi
    done
    return 1
}

homeboy_disposable_local_db_random_password() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 16
        return 0
    fi
    od -An -tx1 -N16 /dev/urandom | tr -d ' \n'
}

homeboy_disposable_local_db_export() {
    local env_name="$1"
    local env_value="$2"

    [ -n "$env_name" ] || return 0
    if [[ ! "$env_name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
        echo "ERROR: invalid environment variable name for disposable local DB: $env_name" >&2
        return 2
    fi
    export "${env_name}=${env_value}"
}

homeboy_disposable_local_db_validate_identifier() {
    local label="$1"
    local value="$2"

    if [[ ! "$value" =~ ^[A-Za-z0-9_]+$ ]]; then
        echo "ERROR: disposable local DB ${label} must contain only letters, numbers, and underscores: $value" >&2
        return 2
    fi
}

homeboy_disposable_local_db_sql_string() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\'/\'\'}"
    printf "%s" "$value"
}

homeboy_disposable_local_db_stop() {
    local pid="${HOMEBOY_DISPOSABLE_LOCAL_DB_PID:-}"
    local dir="${HOMEBOY_DISPOSABLE_LOCAL_DB_DIR:-}"

    if [ -n "$pid" ] && kill -0 "$pid" >/dev/null 2>&1; then
        kill "$pid" >/dev/null 2>&1 || true
        local waited=0
        while kill -0 "$pid" >/dev/null 2>&1 && [ "$waited" -lt 50 ]; do
            sleep 0.1
            waited=$((waited + 1))
        done
        if kill -0 "$pid" >/dev/null 2>&1; then
            kill -9 "$pid" >/dev/null 2>&1 || true
        fi
    fi

    if [ -n "$dir" ] && [ -d "$dir" ]; then
        rm -rf "$dir"
    fi

    HOMEBOY_DISPOSABLE_LOCAL_DB_PID=""
    HOMEBOY_DISPOSABLE_LOCAL_DB_DIR=""
    HOMEBOY_DISPOSABLE_LOCAL_DB_SOCKET=""
}

homeboy_disposable_local_db_init_datadir() {
    local datadir="$1"
    local init_log="$2"
    local init_cmd server_cmd

    if init_cmd=$(homeboy_disposable_local_db_find_command mariadb-install-db mysql_install_db); then
        "$init_cmd" --datadir="$datadir" --auth-root-authentication-method=normal --skip-test-db >"$init_log" 2>&1
        return $?
    fi

    if server_cmd=$(homeboy_disposable_local_db_find_command mariadbd mysqld); then
        "$server_cmd" --no-defaults --initialize-insecure --datadir="$datadir" >"$init_log" 2>&1
        return $?
    fi

    echo "ERROR: no MySQL/MariaDB initializer found. Install mariadb-install-db, mysql_install_db, mariadbd, or mysqld." >&2
    return 2
}

homeboy_disposable_local_db_start() {
    local database="homeboy_test"
    local user="homeboy"
    local password=""
    local tmp_parent="${TMPDIR:-/tmp}"
    local env_host=""
    local env_port=""
    local env_socket=""
    local env_database=""
    local env_user=""
    local env_password=""

    while [ "$#" -gt 0 ]; do
        case "$1" in
            --database) database="${2:-}"; shift 2 ;;
            --user) user="${2:-}"; shift 2 ;;
            --password) password="${2:-}"; shift 2 ;;
            --tmp-dir) tmp_parent="${2:-}"; shift 2 ;;
            --env-host) env_host="${2:-}"; shift 2 ;;
            --env-port) env_port="${2:-}"; shift 2 ;;
            --env-socket) env_socket="${2:-}"; shift 2 ;;
            --env-database) env_database="${2:-}"; shift 2 ;;
            --env-user) env_user="${2:-}"; shift 2 ;;
            --env-password) env_password="${2:-}"; shift 2 ;;
            --help|-h) homeboy_disposable_local_db_usage; return 0 ;;
            *) echo "ERROR: unknown disposable local DB option: $1" >&2; homeboy_disposable_local_db_usage >&2; return 2 ;;
        esac
    done

    if [ -n "${HOMEBOY_DISPOSABLE_LOCAL_DB_PID:-}" ] && kill -0 "$HOMEBOY_DISPOSABLE_LOCAL_DB_PID" >/dev/null 2>&1; then
        echo "ERROR: disposable local DB is already running in this shell (pid ${HOMEBOY_DISPOSABLE_LOCAL_DB_PID})." >&2
        return 2
    fi

    if [ -z "$database" ] || [ -z "$user" ]; then
        echo "ERROR: disposable local DB database and user must be non-empty." >&2
        return 2
    fi
    homeboy_disposable_local_db_validate_identifier "database" "$database" || return $?
    homeboy_disposable_local_db_validate_identifier "user" "$user" || return $?

    local server_cmd client_cmd admin_cmd
    server_cmd=$(homeboy_disposable_local_db_find_command mariadbd mysqld) || { echo "ERROR: no MySQL/MariaDB server binary found. Install mariadbd or mysqld." >&2; return 2; }
    client_cmd=$(homeboy_disposable_local_db_find_command mariadb mysql) || { echo "ERROR: no MySQL/MariaDB client found. Install mariadb or mysql." >&2; return 2; }
    admin_cmd=$(homeboy_disposable_local_db_find_command mariadb-admin mysqladmin) || { echo "ERROR: no MySQL/MariaDB admin client found. Install mariadb-admin or mysqladmin." >&2; return 2; }

    password="${password:-$(homeboy_disposable_local_db_random_password)}"

    local run_dir datadir socket pid_file log_file init_log
    run_dir=$(mktemp -d "${tmp_parent%/}/homeboy-local-db.XXXXXX") || return 1
    datadir="${run_dir}/data"
    socket="${run_dir}/mysql.sock"
    pid_file="${run_dir}/mysql.pid"
    log_file="${run_dir}/mysql.log"
    init_log="${run_dir}/mysql-init.log"
    mkdir -p "$datadir"

    if ! homeboy_disposable_local_db_init_datadir "$datadir" "$init_log"; then
        echo "ERROR: failed to initialize disposable local DB datadir. See $init_log" >&2
        rm -rf "$run_dir"
        return 1
    fi

    "$server_cmd" --no-defaults --datadir="$datadir" --socket="$socket" --pid-file="$pid_file" --skip-networking --log-error="$log_file" --innodb-flush-method=normal >/dev/null 2>&1 &
    local pid=$!

    HOMEBOY_DISPOSABLE_LOCAL_DB_DIR="$run_dir"
    HOMEBOY_DISPOSABLE_LOCAL_DB_PID="$pid"
    HOMEBOY_DISPOSABLE_LOCAL_DB_SOCKET="$socket"
    HOMEBOY_DISPOSABLE_LOCAL_DB_CLIENT="$client_cmd"

    local waited=0
    until "$admin_cmd" --protocol=socket --socket="$socket" -uroot ping >/dev/null 2>&1; do
        if ! kill -0 "$pid" >/dev/null 2>&1; then
            echo "ERROR: disposable local DB exited during startup. See $log_file" >&2
            homeboy_disposable_local_db_stop
            return 1
        fi
        waited=$((waited + 1))
        if [ "$waited" -gt 200 ]; then
            echo "ERROR: timed out waiting for disposable local DB startup. See $log_file" >&2
            homeboy_disposable_local_db_stop
            return 1
        fi
        sleep 0.1
    done

    local sql_password
    sql_password=$(homeboy_disposable_local_db_sql_string "$password")
    if ! "$client_cmd" --protocol=socket --socket="$socket" -uroot <<SQL
CREATE DATABASE \`${database}\`;
CREATE USER '${user}'@'localhost' IDENTIFIED BY '${sql_password}';
GRANT ALL PRIVILEGES ON \`${database}\`.* TO '${user}'@'localhost';
FLUSH PRIVILEGES;
SQL
    then
        echo "ERROR: failed to create disposable local DB credentials. See $log_file" >&2
        homeboy_disposable_local_db_stop
        return 1
    fi

    homeboy_disposable_local_db_export "$env_host" "localhost"
    homeboy_disposable_local_db_export "$env_port" "0"
    homeboy_disposable_local_db_export "$env_socket" "$socket"
    homeboy_disposable_local_db_export "$env_database" "$database"
    homeboy_disposable_local_db_export "$env_user" "$user"
    homeboy_disposable_local_db_export "$env_password" "$password"

    if [ "${HOMEBOY_DISPOSABLE_LOCAL_DB_STOP_REGISTERED:-0}" != "1" ]; then
        trap homeboy_disposable_local_db_stop EXIT
        HOMEBOY_DISPOSABLE_LOCAL_DB_STOP_REGISTERED=1
    fi
}
