<?php
// Lightweight SQLite database driver for WordPress testing
// Minimal implementation to allow WP tests to run using PDO-backed SQLite in module environment

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
                $count = $this->pdo->exec( $query );
                if ( $count === false ) {
                    $error = $this->pdo->errorInfo();
                    $this->last_error = isset($error[2]) ? $error[2] : 'Unknown PDO error';
                    return false;
                }

                $this->num_queries++;
                $this->rows_affected = $count;
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

// Read database configuration from environment (set by wp-config.php)
$dbuser = getenv('DB_USER') ?: 'root';
$dbpassword = getenv('DB_PASSWORD') ?: '';
$dbname = getenv('DB_NAME') ?: ':memory:';
$dbhost = getenv('DB_HOST') ?: 'localhost';

// Instantiate and expose global wpdb instance to WordPress
$GLOBALS['wpdb'] = new SQLite_DB($dbuser, $dbpassword, $dbname, $dbhost);
