<?php
// Lightweight SQLite database driver for WordPress testing
// Minimal implementation to allow WP tests to run using PDO-backed SQLite in extension environment

if ( ! extension_loaded( 'pdo_sqlite' ) ) {
    echo "Fatal: pdo_sqlite extension is not loaded. Install php-sqlite3 and restart PHP.\n";
    exit( 1 );
}

// Fast hack: provide a fake mysqli subclass so procedural mysqli_* calls accept the object
class FakeMySQL extends mysqli {
    // Override constructor to avoid attempting a real connection
    public function __construct() {
        // do nothing
    }

    // Provide a server info string property used by some mysqli functions
    public function server_info() {
        return '5.7.999';
    }

    public function __destruct() {
        // noop
    }
}

class SQLite_DB extends wpdb {
    /** @var PDO|null */
    public $pdo = null;

    public function __construct($dbuser, $dbpassword, $dbname, $dbhost) {
        // Initialize as PDO-backed DB handle to support SQLite in tests
        if ($dbname === ':memory:') {
            $this->pdo = new PDO('sqlite::memory:');
        } else {
            $this->pdo = new PDO("sqlite:$dbname");
        }

        $this->pdo->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);

        // Provide a fake mysqli object so procedural mysqli_* calls do not TypeError
        $this->dbh = new FakeMySQL();

        // Mark that this is not a MySQL connection
        $this->is_mysql = false;
        $this->ready = true;

        // Initialize some properties expected by WP
        $this->last_error = '';
        $this->last_result = array();
        $this->num_rows = 0;
        $this->rows_affected = 0;
        $this->insert_id = 0;
    }

    // Match signature of wpdb::select
    public function select( $db = null, $dbh = null ) {
        if ( $this->pdo instanceof PDO ) {
            // For SQLite we treat selecting a database as a no-op
            $this->dbname = $db;
            return true;
        }
        return parent::select( $db, $dbh );
    }

    /**
     * Translate MySQL CREATE TABLE DDL to SQLite-compatible syntax.
     *
     * WordPress generates MySQL-specific DDL (AUTO_INCREMENT, ENGINE=InnoDB,
     * sized integers, KEY/UNIQUE KEY syntax) that SQLite cannot parse.
     * This method rewrites the SQL so SQLite can execute it.
     *
     * @param string $query MySQL CREATE TABLE statement.
     * @return string SQLite-compatible CREATE TABLE + CREATE INDEX statements.
     */
    private function translate_create_table( $query ) {
        // Strip MySQL table options after the closing parenthesis
        // (ENGINE=InnoDB, DEFAULT CHARSET=..., COLLATE=..., AUTO_INCREMENT=N)
        $cleaned = preg_replace( '/\)\s*(?:ENGINE|DEFAULT|COLLATE|AUTO_INCREMENT)\b.*/is', ')', $query );

        // Extract table name and body
        if ( ! preg_match( '/CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?[`"]?(\w+)[`"]?\s*\((.+)\)\s*$/is', $cleaned, $m ) ) {
            return $query; // Not a recognizable CREATE TABLE — return as-is
        }

        $table_name = $m[1];
        $body       = $m[2];

        // Split body into column/constraint definitions (respecting parentheses)
        $defs        = array();
        $depth       = 0;
        $current     = '';

        for ( $i = 0; $i < strlen( $body ); $i++ ) {
            $ch = $body[ $i ];
            if ( $ch === '(' ) {
                $depth++;
                $current .= $ch;
            } elseif ( $ch === ')' ) {
                $depth--;
                $current .= $ch;
            } elseif ( $ch === ',' && $depth === 0 ) {
                $defs[] = trim( $current );
                $current = '';
            } else {
                $current .= $ch;
            }
        }
        if ( trim( $current ) !== '' ) {
            $defs[] = trim( $current );
        }

        $columns = array();
        $indexes = array();

        // First pass: detect if any column has AUTO_INCREMENT (determines PRIMARY KEY handling)
        $has_autoincrement = false;
        foreach ( $defs as $def ) {
            if ( preg_match( '/AUTO_INCREMENT/i', $def ) ) {
                $has_autoincrement = true;
                break;
            }
        }

        foreach ( $defs as $def ) {
            // PRIMARY KEY constraint — skip if an AUTOINCREMENT column already has it inline
            if ( preg_match( '/^\s*PRIMARY\s+KEY\s*\((.+)\)/i', $def, $pk ) ) {
                if ( ! $has_autoincrement ) {
                    $columns[] = 'PRIMARY KEY (' . $pk[1] . ')';
                }
                continue;
            }

            // UNIQUE KEY → extract for CREATE UNIQUE INDEX
            if ( preg_match( '/^\s*UNIQUE\s+KEY\s+[`"]?(\w+)[`"]?\s*\((.+)\)/i', $def, $uk ) ) {
                $indexes[] = "CREATE UNIQUE INDEX IF NOT EXISTS {$uk[1]} ON {$table_name} ({$uk[2]})";
                continue;
            }

            // KEY (non-unique index) → extract for CREATE INDEX
            if ( preg_match( '/^\s*KEY\s+[`"]?(\w+)[`"]?\s*\((.+)\)/i', $def, $k ) ) {
                // Strip column length specifiers like col(191) → col
                $idx_cols = preg_replace( '/(\w+)\(\d+\)/', '$1', $k[2] );
                $indexes[] = "CREATE INDEX IF NOT EXISTS {$k[1]} ON {$table_name} ({$idx_cols})";
                continue;
            }

            // FULLTEXT KEY → skip (SQLite doesn't support FULLTEXT; not needed for tests)
            if ( preg_match( '/^\s*FULLTEXT\s+KEY\b/i', $def ) ) {
                continue;
            }

            // Column definition — translate types
            $col = $def;

            // Detect AUTO_INCREMENT column to use INTEGER PRIMARY KEY AUTOINCREMENT
            $is_auto = false;
            if ( preg_match( '/AUTO_INCREMENT/i', $col ) ) {
                $is_auto = true;
                $col = preg_replace( '/\s*AUTO_INCREMENT/i', '', $col );
            }

            // Integer types: bigint(20) unsigned, int(11), mediumint(9), smallint(6), tinyint(1) → INTEGER
            $col = preg_replace( '/\b(big|medium|small|tiny)?int\(\d+\)\s*(unsigned\s*)?/i', 'INTEGER ', $col );
            $col = preg_replace( '/\bINTEGER\s+unsigned\b/i', 'INTEGER', $col );

            // varchar(N) → TEXT
            $col = preg_replace( '/\bvarchar\(\d+\)/i', 'TEXT', $col );

            // longtext, mediumtext, tinytext → TEXT
            $col = preg_replace( '/\b(long|medium|tiny)text\b/i', 'TEXT', $col );

            // longblob, mediumblob, tinyblob, blob → BLOB
            $col = preg_replace( '/\b(long|medium|tiny)?blob\b/i', 'BLOB', $col );

            // datetime, date, timestamp → TEXT
            $col = preg_replace( '/\b(datetime|timestamp)\b/i', 'TEXT', $col );

            // decimal(N,M), float, double → REAL
            $col = preg_replace( '/\b(decimal|numeric)\(\d+,\s*\d+\)/i', 'REAL', $col );
            $col = preg_replace( '/\b(float|double)\b/i', 'REAL', $col );

            // Remove COLLATE and CHARACTER SET clauses
            $col = preg_replace( '/\s+COLLATE\s+\S+/i', '', $col );
            $col = preg_replace( '/\s+CHARACTER\s+SET\s+\S+/i', '', $col );

            // If AUTO_INCREMENT, make it INTEGER PRIMARY KEY AUTOINCREMENT
            if ( $is_auto ) {
                // Remove any standalone NOT NULL (AUTOINCREMENT implies it)
                $col = preg_replace( '/\s+NOT\s+NULL/i', '', $col );
                // Extract column name
                if ( preg_match( '/^[`"]?(\w+)[`"]?\s+INTEGER/i', $col, $cn ) ) {
                    $col = $cn[1] . ' INTEGER PRIMARY KEY AUTOINCREMENT';
                }
            }

            $columns[] = $col;
        }

        // Build the CREATE TABLE statement
        $create = "CREATE TABLE IF NOT EXISTS {$table_name} (\n  " . implode( ",\n  ", $columns ) . "\n)";

        // Return CREATE TABLE followed by any extracted index statements
        if ( ! empty( $indexes ) ) {
            return $create . ";\n" . implode( ";\n", $indexes );
        }

        return $create;
    }

    /**
     * Translate MySQL ALTER TABLE to SQLite-compatible form.
     *
     * SQLite has very limited ALTER TABLE support (only ADD COLUMN and RENAME).
     * Unsupported operations (MODIFY, CHANGE, DROP COLUMN on old SQLite) are
     * silently skipped with a warning to stderr.
     *
     * @param string $query MySQL ALTER TABLE statement.
     * @return string|null Translated query, or null if unsupported.
     */
    private function translate_alter_table( $query ) {
        // ADD COLUMN — supported
        if ( preg_match( '/ALTER\s+TABLE\s+\S+\s+ADD\s+(COLUMN\s+)?\S+/i', $query ) ) {
            // Translate column types in the ADD clause
            $q = $query;
            $q = preg_replace( '/\b(big|medium|small|tiny)?int\(\d+\)\s*(unsigned)?/i', 'INTEGER', $q );
            $q = preg_replace( '/\bINTEGER\s+unsigned\b/i', 'INTEGER', $q );
            $q = preg_replace( '/\bvarchar\(\d+\)/i', 'TEXT', $q );
            $q = preg_replace( '/\b(long|medium|tiny)text\b/i', 'TEXT', $q );
            $q = preg_replace( '/\b(datetime|timestamp)\b/i', 'TEXT', $q );
            $q = preg_replace( '/\b(decimal|numeric)\(\d+,\s*\d+\)/i', 'REAL', $q );
            $q = preg_replace( '/\b(float|double)\b/i', 'REAL', $q );
            $q = preg_replace( '/\s+COLLATE\s+\S+/i', '', $q );
            $q = preg_replace( '/\s+CHARACTER\s+SET\s+\S+/i', '', $q );
            $q = preg_replace( '/\s+AUTO_INCREMENT/i', '', $q );
            $q = preg_replace( '/\s+AFTER\s+\S+/i', '', $q );
            $q = preg_replace( '/\s+FIRST\b/i', '', $q );
            return $q;
        }

        // RENAME TABLE — supported
        if ( preg_match( '/ALTER\s+TABLE\s+\S+\s+RENAME\s+TO\s+/i', $query ) ) {
            return $query;
        }

        // Anything else (MODIFY, CHANGE, DROP COLUMN, ADD INDEX, etc.) — skip
        fwrite( STDERR, "[SQLite_DB] Skipping unsupported ALTER TABLE: " . substr( $query, 0, 120 ) . "\n" );
        return null;
    }

    /**
     * Run a query using PDO. Mirrors wpdb::query semantics where practical.
     */
    public function query( $query ) {
        if ( ! ( $this->pdo instanceof PDO ) ) {
            return parent::query( $query );
        }

        $this->last_error = '';
        $this->last_result = array();
        $this->num_rows = 0;
        $this->rows_affected = 0;
        $this->insert_id = 0;

        try {
            // Use exec for statements that do not return results
            $trimmed = ltrim($query);
            if ( preg_match( '/^\s*(insert|update|delete|replace|create|alter|truncate|drop)\b/i', $trimmed ) ) {

                // Translate MySQL DDL to SQLite-compatible syntax
                $is_create_table = false;
                if ( preg_match( '/^\s*CREATE\s+TABLE\b/i', $trimmed ) ) {
                    $query = $this->translate_create_table( $query );
                    $is_create_table = true;
                } elseif ( preg_match( '/^\s*ALTER\s+TABLE\b/i', $trimmed ) ) {
                    $query = $this->translate_alter_table( $query );
                    if ( $query === null ) {
                        // Unsupported ALTER — pretend success
                        $this->num_queries++;
                        return 0;
                    }
                }

                // CREATE TABLE translation may produce multiple statements
                // (CREATE TABLE + CREATE INDEX separated by semicolons).
                // Only split on semicolons for translated DDL to avoid breaking
                // INSERT/UPDATE values that contain semicolons.
                if ( $is_create_table && strpos( $query, ';' ) !== false ) {
                    $statements = array_filter( array_map( 'trim', explode( ';', $query ) ) );
                } else {
                    $statements = array( $query );
                }

                $total_affected = 0;
                foreach ( $statements as $stmt ) {
                    $count = $this->pdo->exec( $stmt );
                    if ( $count === false ) {
                        $error = $this->pdo->errorInfo();
                        $this->last_error = isset($error[2]) ? $error[2] : 'Unknown PDO error';
                        return false;
                    }
                    $total_affected += $count;
                }

                $this->num_queries++;
                $this->rows_affected = $total_affected;
                if ( preg_match( '/^\s*(insert|replace)\b/i', $trimmed ) ) {
                    try {
                        $last = $this->pdo->lastInsertId();
                        $this->insert_id = $last === false ? 0 : (int) $last;
                    } catch ( Exception $e ) {
                        $this->insert_id = 0;
                    }
                }

                return $this->rows_affected;
            }

            $stmt = $this->pdo->query( $query );
            if ( $stmt === false ) {
                $error = $this->pdo->errorInfo();
                $this->last_error = isset($error[2]) ? $error[2] : 'Unknown PDO error';
                return false;
            }

            $rows = $stmt->fetchAll(PDO::FETCH_OBJ);
            $this->last_result = $rows;
            $this->num_rows = count($rows);
            $this->result = $stmt;
            $this->num_queries++;

            return $this->num_rows;

        } catch ( PDOException $e ) {
            $this->last_error = $e->getMessage();
            return false;
        }
    }

    /**
     * Escape string using PDO quote when available.
     */
    public function _real_escape( $data ) {
        if ( ! is_scalar( $data ) ) {
            return '';
        }

        if ( $this->pdo instanceof PDO ) {
            $quoted = $this->pdo->quote( $data );
            if ( $quoted === false ) {
                return addslashes( $data );
            }
            if ( strlen( $quoted ) >= 2 && $quoted[0] === "'" && $quoted[strlen( $quoted ) - 1] === "'" ) {
                return substr( $quoted, 1, -1 );
            }
            return $quoted;
        }

        return parent::_real_escape( $data );
    }

    public function get_var( $query = null, $x = 0, $y = 0 ) {
        if ( ! ( $this->pdo instanceof PDO ) ) {
            return parent::get_var( $query, $x, $y );
        }

        if ( $query !== null ) {
            $this->query( $query );
        }

        if ( empty( $this->last_result ) ) {
            return null;
        }

        $row = $this->last_result[0];
        $values = array_values( (array) $row );
        return isset( $values[ $x ] ) ? $values[ $x ] : null;
    }

    public function get_row( $query = null, $output = OBJECT, $y = 0 ) {
        if ( ! ( $this->pdo instanceof PDO ) ) {
            return parent::get_row( $query, $output, $y );
        }

        if ( $query !== null ) {
            $this->query( $query );
        }

        if ( empty( $this->last_result ) ) {
            return null;
        }

        $row = $this->last_result[ $y ] ?? null;
        if ( $row === null ) {
            return null;
        }

        if ( $output === OBJECT ) {
            return $row;
        } elseif ( $output === ARRAY_A ) {
            return (array) $row;
        } elseif ( $output === ARRAY_N ) {
            return array_values( (array) $row );
        }

        return $row;
    }

    public function get_results( $query = null, $output = OBJECT ) {
        if ( ! ( $this->pdo instanceof PDO ) ) {
            return parent::get_results( $query, $output );
        }

        if ( $query !== null ) {
            $this->query( $query );
        }

        $results = $this->last_result;

        if ( $output === OBJECT ) {
            return $results;
        }

        $out = array();
        foreach ( $results as $r ) {
            if ( $output === ARRAY_A ) {
                $out[] = (array) $r;
            } elseif ( $output === ARRAY_N ) {
                $out[] = array_values( (array) $r );
            }
        }

        return $out;
    }

    /**
     * Return a fake server info string that mimics MySQL for version checks.
     */
    public function db_server_info() {
        return '5.7.999';
    }

    public function db_version() {
        // Return a MySQL-compatible version string so WordPress passes version checks
        return '5.7.999';
    }

    // Match signature of wpdb::check_connection
    public function check_connection( $allow_bail = true ) {
        // With PDO we assume connection is OK (throwing exceptions otherwise)
        return true;
    }
}

// Read database configuration from PHP constants (defined by wp-config.php / wp-tests-config.php).
// Constants are always available by the time this drop-in is loaded via require_wp_db().
$dbuser     = defined( 'DB_USER' ) ? DB_USER : '';
$dbpassword = defined( 'DB_PASSWORD' ) ? DB_PASSWORD : '';
$dbname     = defined( 'DB_NAME' ) ? DB_NAME : ':memory:';
$dbhost     = defined( 'DB_HOST' ) ? DB_HOST : '';

// Instantiate and expose global wpdb instance to WordPress
$GLOBALS['wpdb'] = new SQLite_DB( $dbuser, $dbpassword, $dbname, $dbhost );
