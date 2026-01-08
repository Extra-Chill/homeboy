import Foundation

/// Executes MySQL queries via CLI through SSH tunnel to remote server.
/// Uses Local by Flywheel's bundled MySQL binary or Homebrew mysql-client.
class MySQLService: @unchecked Sendable {
    
    private let username: String
    private let password: String
    private let database: String
    
    /// Manages the SSH tunnel to remote server
    private var tunnelService: SSHTunnelService?
    
    init() throws {
        // Read non-secrets from ConfigurationManager (JSON config)
        // Use static nonisolated method for thread-safe access from non-MainActor context
        let siteConfig = ConfigurationManager.readCurrentProject()
        let username = siteConfig.database.user
        let database = siteConfig.database.name
        
        // Password stays in Keychain
        let creds = KeychainService.getLiveMySQLCredentials()
        guard !username.isEmpty,
              let password = creds.password,
              !database.isEmpty else {
            throw MySQLError.notConfigured
        }
        self.username = username
        self.password = password
        self.database = database
    }
    
    /// Connects to the database (establishes SSH tunnel)
    func connect() async throws {
        guard let tunnel = SSHTunnelService() else {
            throw MySQLError.tunnelFailed("SSH not configured")
        }
        self.tunnelService = tunnel
        
        let result = await tunnel.connect()
        switch result {
        case .success:
            break
        case .failure(let error):
            throw error
        }
        
        // Test the connection with a simple query
        _ = try await executeQuery("SELECT 1")
    }
    
    /// Disconnects from the database (closes SSH tunnel)
    func disconnect() {
        tunnelService?.disconnect()
        tunnelService = nil
    }
    
    /// Fetches all tables in the database with metadata
    func fetchTables() async throws -> [DatabaseTable] {
        let query = "SHOW TABLE STATUS"
        let output = try await executeQuery(query)
        return parseTableStatus(output)
    }
    
    /// Fetches column information for a specific table
    func fetchColumns(table: String) async throws -> [DatabaseColumn] {
        let query = "DESCRIBE `\(escapeIdentifier(table))`"
        let output = try await executeQuery(query)
        return parseDescribe(output)
    }
    
    /// Fetches rows from a table with pagination
    func fetchRows(table: String, limit: Int, offset: Int) async throws -> [TableRow] {
        let query = "SELECT * FROM `\(escapeIdentifier(table))` LIMIT \(limit) OFFSET \(offset)"
        let output = try await executeQuery(query)
        return parseSelectResults(output)
    }
    
    /// Gets the total row count for a table
    func getRowCount(table: String) async throws -> Int {
        let query = "SELECT COUNT(*) as count FROM `\(escapeIdentifier(table))`"
        let output = try await executeQuery(query)
        
        // Parse the count from output
        let lines = output.components(separatedBy: "\n").filter { !$0.isEmpty }
        if lines.count >= 1 {
            let countLine = lines[0].trimmingCharacters(in: .whitespaces)
            if let count = Int(countLine) {
                return count
            }
        }
        return 0
    }
    
    // MARK: - Private Methods
    
    /// Finds the MySQL binary (Local by Flywheel or Homebrew)
    private func findMySQLBinary() -> String? {
        // Try Local by Flywheel first
        if let localBin = LocalEnvironment.detectMySQLBinDirectory() {
            let localMysql = "\(localBin)/mysql"
            if FileManager.default.fileExists(atPath: localMysql) {
                return localMysql
            }
        }
        
        // Try Homebrew mysql-client
        let homebrewPaths = [
            "/opt/homebrew/opt/mysql-client/bin/mysql",  // Apple Silicon
            "/usr/local/opt/mysql-client/bin/mysql",     // Intel
            "/opt/homebrew/bin/mysql",
            "/usr/local/bin/mysql"
        ]
        
        for path in homebrewPaths {
            if FileManager.default.fileExists(atPath: path) {
                return path
            }
        }
        
        return nil
    }
    
    /// Executes a MySQL query and returns the raw output
    private func executeQuery(_ query: String) async throws -> String {
        guard let mysqlPath = findMySQLBinary() else {
            throw MySQLError.mysqlNotFound
        }
        
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async { [self] in
                let process = Process()
                process.executableURL = URL(fileURLWithPath: mysqlPath)
                
                // Connect via TCP through SSH tunnel
                let arguments = [
                    "-h", "127.0.0.1",
                    "-P", "\(SSHTunnelService.localPort)",
                    "-u", username,
                    "-p\(password)",
                    "-D", database,
                    "-N",  // Skip column names in results
                    "-B",  // Batch mode (tab-separated)
                    "-e", query
                ]
                
                process.arguments = arguments
                
                // Set up environment for Local by Flywheel (if using their mysql binary)
                if let env = LocalEnvironment.buildEnvironment() {
                    process.environment = env
                }
                
                let outputPipe = Pipe()
                let errorPipe = Pipe()
                process.standardOutput = outputPipe
                process.standardError = errorPipe
                
                do {
                    try process.run()
                    process.waitUntilExit()
                    
                    let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
                    let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
                    
                    let output = String(data: outputData, encoding: .utf8) ?? ""
                    let errorOutput = String(data: errorData, encoding: .utf8) ?? ""
                    
                    if process.terminationStatus == 0 {
                        continuation.resume(returning: output)
                    } else {
                        let errorMessage = errorOutput.isEmpty ? "Query failed with exit code \(process.terminationStatus)" : errorOutput
                        continuation.resume(throwing: MySQLError.queryFailed(errorMessage.trimmingCharacters(in: .whitespacesAndNewlines)))
                    }
                } catch {
                    continuation.resume(throwing: MySQLError.queryFailed(error.localizedDescription))
                }
            }
        }
    }
    
    /// Escapes a MySQL identifier (table/column name)
    private func escapeIdentifier(_ identifier: String) -> String {
        identifier.replacingOccurrences(of: "`", with: "``")
    }
    
    /// Escapes a value for use in SQL queries
    private func escapeValue(_ value: String) -> String {
        value
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "'", with: "\\'")
            .replacingOccurrences(of: "\"", with: "\\\"")
    }
    
    // MARK: - Destructive Operations
    
    /// Deletes a single row from a table by primary key
    func deleteRow(table: String, primaryKeyColumn: String, primaryKeyValue: String) async throws {
        let query = "DELETE FROM `\(escapeIdentifier(table))` WHERE `\(escapeIdentifier(primaryKeyColumn))` = '\(escapeValue(primaryKeyValue))' LIMIT 1"
        _ = try await executeQuery(query)
    }
    
    /// Drops an entire table from the database
    func dropTable(table: String) async throws {
        let query = "DROP TABLE `\(escapeIdentifier(table))`"
        _ = try await executeQuery(query)
    }
    
    // MARK: - Output Parsing
    
    /// Parses SHOW TABLE STATUS output into DatabaseTable objects
    private func parseTableStatus(_ output: String) -> [DatabaseTable] {
        var tables: [DatabaseTable] = []
        let lines = output.components(separatedBy: "\n").filter { !$0.isEmpty }
        
        for line in lines {
            let columns = line.components(separatedBy: "\t")
            // SHOW TABLE STATUS columns: Name, Engine, Version, Row_format, Rows, Avg_row_length, Data_length, ...
            if columns.count >= 7 {
                let name = columns[0]
                let engine = columns[1]
                let rowCount = Int(columns[4]) ?? 0
                let dataLength = Int64(columns[6]) ?? 0
                
                tables.append(DatabaseTable(
                    name: name,
                    rowCount: rowCount,
                    engine: engine,
                    dataLength: dataLength
                ))
            }
        }
        
        return tables.sorted { $0.name < $1.name }
    }
    
    /// Parses DESCRIBE output into DatabaseColumn objects
    private func parseDescribe(_ output: String) -> [DatabaseColumn] {
        var columns: [DatabaseColumn] = []
        let lines = output.components(separatedBy: "\n").filter { !$0.isEmpty }
        
        for line in lines {
            let parts = line.components(separatedBy: "\t")
            // DESCRIBE columns: Field, Type, Null, Key, Default, Extra
            if parts.count >= 5 {
                let name = parts[0]
                let type = parts[1]
                let isNullable = parts[2].uppercased() == "YES"
                let isPrimaryKey = parts[3].uppercased() == "PRI"
                let defaultValue = parts[4] == "NULL" ? nil : parts[4]
                
                columns.append(DatabaseColumn(
                    name: name,
                    type: type,
                    isNullable: isNullable,
                    isPrimaryKey: isPrimaryKey,
                    defaultValue: defaultValue
                ))
            }
        }
        
        return columns
    }
    
    /// Parses SELECT output into TableRow objects
    private func parseSelectResults(_ output: String) -> [TableRow] {
        var rows: [TableRow] = []
        let lines = output.components(separatedBy: "\n").filter { !$0.isEmpty }
        
        for (index, line) in lines.enumerated() {
            let values = line.components(separatedBy: "\t")
            var rowValues: [String: String?] = [:]
            
            // Since we use -N (no headers), we need column indices
            // We'll use numeric keys and the ViewModel will map them to column names
            for (colIndex, value) in values.enumerated() {
                let key = "col_\(colIndex)"
                rowValues[key] = value == "NULL" ? nil : value
            }
            
            rows.append(TableRow(id: index, values: rowValues))
        }
        
        return rows
    }
    
    /// Fetches rows with column names (for proper mapping)
    func fetchRowsWithColumns(table: String, columns: [DatabaseColumn], limit: Int, offset: Int) async throws -> [TableRow] {
        let query = "SELECT * FROM `\(escapeIdentifier(table))` LIMIT \(limit) OFFSET \(offset)"
        let output = try await executeQuery(query)
        
        var rows: [TableRow] = []
        let lines = output.components(separatedBy: "\n").filter { !$0.isEmpty }
        
        for (index, line) in lines.enumerated() {
            let values = line.components(separatedBy: "\t")
            var rowValues: [String: String?] = [:]
            
            // Map values to column names
            for (colIndex, column) in columns.enumerated() {
                if colIndex < values.count {
                    let value = values[colIndex]
                    rowValues[column.name] = value == "NULL" ? nil : value
                }
            }
            
            rows.append(TableRow(id: index, values: rowValues))
        }
        
        return rows
    }
    
    /// Executes a custom SQL query and returns column names and row data.
    /// Unlike other methods, this includes column headers in the output.
    func executeCustomQuery(_ query: String) async throws -> (columns: [String], rows: [[String]]) {
        guard let mysqlPath = findMySQLBinary() else {
            throw MySQLError.mysqlNotFound
        }
        
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async { [self] in
                let process = Process()
                process.executableURL = URL(fileURLWithPath: mysqlPath)
                
                // Connect via TCP through SSH tunnel
                // Note: No -N flag so we get column headers
                let arguments = [
                    "-h", "127.0.0.1",
                    "-P", "\(SSHTunnelService.localPort)",
                    "-u", username,
                    "-p\(password)",
                    "-D", database,
                    "-B",  // Batch mode (tab-separated)
                    "-e", query
                ]
                
                process.arguments = arguments
                
                if let env = LocalEnvironment.buildEnvironment() {
                    process.environment = env
                }
                
                let outputPipe = Pipe()
                let errorPipe = Pipe()
                process.standardOutput = outputPipe
                process.standardError = errorPipe
                
                do {
                    try process.run()
                    process.waitUntilExit()
                    
                    let outputData = outputPipe.fileHandleForReading.readDataToEndOfFile()
                    let errorData = errorPipe.fileHandleForReading.readDataToEndOfFile()
                    
                    let output = String(data: outputData, encoding: .utf8) ?? ""
                    let errorOutput = String(data: errorData, encoding: .utf8) ?? ""
                    

                    if process.terminationStatus == 0 {
                        // Parse output: first line is column headers, rest are data rows
                        let lines = output.components(separatedBy: "\n").filter { !$0.isEmpty }
                        
                        if lines.isEmpty {
                            // No results (e.g., UPDATE/INSERT/DELETE)
                            continuation.resume(returning: ([], []))
                            return
                        }
                        
                        // First line contains column names
                        let columnNames = lines[0].components(separatedBy: "\t")
                        
                        // Remaining lines are data rows
                        var rows: [[String]] = []
                        for i in 1..<lines.count {
                            let values = lines[i].components(separatedBy: "\t")
                            rows.append(values)
                        }
                        
                        continuation.resume(returning: (columnNames, rows))
                    } else {
                        let errorMessage = errorOutput.isEmpty ? "Query failed with exit code \(process.terminationStatus)" : errorOutput
                        continuation.resume(throwing: MySQLError.queryFailed(errorMessage.trimmingCharacters(in: .whitespacesAndNewlines)))
                    }
                } catch {
                    continuation.resume(throwing: MySQLError.queryFailed(error.localizedDescription))
                }
            }
        }
    }
}
