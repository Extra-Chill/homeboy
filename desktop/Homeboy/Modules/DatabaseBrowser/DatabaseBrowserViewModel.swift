import AppKit
import Combine
import Foundation
import SwiftUI

/// ViewModel for the Database Browser module
@MainActor
class DatabaseBrowserViewModel: ObservableObject, ConfigurationObserving {

    var cancellables = Set<AnyCancellable>()

    // MARK: - Published State

    @Published var connectionStatus: DatabaseConnectionStatus = .disconnected

    // Tables (flat list)
    @Published var tables: [DatabaseTable] = []

    // Selected table
    @Published var selectedTable: DatabaseTable?
    @Published var columns: [DatabaseColumn] = []
    @Published var rows: [TableRow] = []
    @Published var selectedRows: Set<Int> = []

    // Pagination
    @Published var currentPage: Int = 1
    @Published var rowsPerPage: Int = 100
    @Published var totalRows: Int = 0

    // Loading state
    @Published var isLoadingTableData: Bool = false

    // Query mode
    @Published var isQueryModeActive: Bool = false
    @Published var customQueryText: String = ""
    @Published var customQueryColumns: [DatabaseColumn] = []
    @Published var customQueryRows: [TableRow] = []
    @Published var selectedQueryRows: Set<Int> = []
    @Published var customQueryError: (any DisplayableError)?
    @Published var isRunningCustomQuery: Bool = false
    @Published var customQueryRowCount: Int = 0
    @Published var hasExecutedQuery: Bool = false

    // Error handling
    @Published var errorMessage: (any DisplayableError)?

    // Row deletion
    @Published var pendingRowDeletion: PendingRowDeletion? = nil
    @Published var showRowDeletionConfirm: Bool = false
    @Published var rowDeletionConfirmText: String = ""
    @Published var isDeletingRow: Bool = false

    // Table deletion
    @Published var pendingTableDeletion: PendingTableDeletion? = nil
    @Published var showTableDeletionConfirm: Bool = false
    @Published var tableDeletionConfirmText: String = ""
    @Published var isDeletingTable: Bool = false

    // MARK: - CLI Bridge

    private let cli = HomeboyCLI.shared

    private var projectId: String {
        ConfigurationManager.shared.safeActiveProject.id
    }

    // MARK: - Initialization

    init() {
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectWillSwitch:
            disconnect()
        default:
            break
        }
    }

    // MARK: - Computed Properties

    var totalPages: Int {
        max(1, Int(ceil(Double(totalRows) / Double(rowsPerPage))))
    }

    var canGoBack: Bool {
        currentPage > 1
    }

    var canGoForward: Bool {
        currentPage < totalPages
    }

    var pageInfo: String {
        let start = (currentPage - 1) * rowsPerPage + 1
        let end = min(currentPage * rowsPerPage, totalRows)
        if totalRows == 0 {
            return "No rows"
        }
        return "Showing \(start)-\(end) of \(totalRows)"
    }

    var isConfigured: Bool {
        cli.isInstalled
    }

    var totalTableCount: Int {
        tables.count
    }

    var canConfirmRowDeletion: Bool {
        rowDeletionConfirmText.lowercased() == "delete"
    }

    var canConfirmTableDeletion: Bool {
        guard let pending = pendingTableDeletion else { return false }
        return tableDeletionConfirmText == pending.table.name
    }

    // MARK: - Connection Methods

    /// Auto-connect when view appears, if configured and not already connected
    func connectIfConfigured() async {
        // Skip if already connected or connecting
        guard connectionStatus == .disconnected else { return }

        // Skip if CLI not installed
        guard isConfigured else { return }

        await connect()
    }

    private func connect() async {
        connectionStatus = .connecting
        errorMessage = nil
    }

    func fetchTables() async {
        guard cli.isInstalled else { return }

        connectionStatus = .connecting
        errorMessage = nil
        isLoadingTableData = true

        do {
            let describe = try await cli.dbDescribe(projectId: projectId, table: nil)

            if describe.success == true, let stdout = describe.stdout {
                tables = parseTables(from: stdout)
            } else {
                errorMessage = AppError("Failed to fetch tables: \(describe.stderr ?? "")", source: "Database Browser")
            }
        } catch {
            errorMessage = AppError("Failed to fetch tables: \(error.localizedDescription)", source: "Database Browser")
        }

        isLoadingTableData = false
    }

    func selectTable(_ table: DatabaseTable) async {
        guard cli.isInstalled else { return }

        connectionStatus = .connecting
        errorMessage = nil
        isLoadingTableData = true

        do {
            let describe = try await cli.dbDescribe(projectId: projectId, table: table.name)
            if describe.success == true, let stdout = describe.stdout {
                columns = parseColumns(from: stdout)
            }

            let countQuery = "SELECT COUNT(*) as count FROM \(table.name)"
            let count = try await cli.dbQuery(projectId: projectId, sql: countQuery)
            if count.success == true, let stdout = count.stdout {
                totalRows = parseRowCount(from: stdout)
            }

            let columnNames = columns.map { $0.name }.joined(separator: ", ")
            let selectQuery = "SELECT \(columnNames.isEmpty ? "*" : columnNames) FROM \(table.name) LIMIT \(rowsPerPage) OFFSET 0"
            let select = try await cli.dbQuery(projectId: projectId, sql: selectQuery)
            if select.success == true, let stdout = select.stdout {
                rows = parseRows(from: stdout, columns: columns)
            }
        } catch {
            errorMessage = AppError("Failed to load table: \(error.localizedDescription)", source: "Database Browser")
        }

        isLoadingTableData = false
    }

    func fetchCurrentPage() async {
        guard cli.isInstalled, let table = selectedTable else { return }

        let offset = (currentPage - 1) * rowsPerPage

        do {
            let columnNames = columns.map { $0.name }.joined(separator: ", ")
            let selectQuery = "SELECT \(columnNames.isEmpty ? "*" : columnNames) FROM \(table.name) LIMIT \(rowsPerPage) OFFSET \(offset)"
            let select = try await cli.dbQuery(projectId: projectId, sql: selectQuery)

            if select.success == true, let stdout = select.stdout {
                rows = parseRows(from: stdout, columns: columns)
            } else {
                errorMessage = AppError("Failed to fetch rows: \(select.stderr ?? "")", source: "Database Browser")
            }
        } catch {
            errorMessage = AppError("Failed to fetch rows: \(error.localizedDescription)", source: "Database Browser")
        }
    }

    // MARK: - JSON Parsing Helpers

    private func parseTables(from jsonOutput: String) -> [DatabaseTable] {
        guard let data = jsonOutput.data(using: .utf8) else { return [] }

        struct WPTable: Decodable {
            let Name: String
            let Rows: String?
            let Engine: String?
        }

        guard let wpTables = try? JSONDecoder().decode([WPTable].self, from: data) else { return [] }

        return wpTables.map { wpTable in
            DatabaseTable(
                name: wpTable.Name,
                rowCount: Int(wpTable.Rows ?? "0") ?? 0,
                engine: wpTable.Engine ?? "InnoDB",
                dataLength: 0
            )
        }
    }

    /// Parse columns from WP-CLI describe output
    private func parseColumns(from jsonOutput: String) -> [DatabaseColumn] {
        // WP-CLI describe returns array of column objects
        guard let data = jsonOutput.data(using: .utf8) else { return [] }

        struct WPColumn: Decodable {
            let Field: String
            let ColumnType: String
            let Null: String?
            let Key: String?
            let Default: String?

            enum CodingKeys: String, CodingKey {
                case Field
                case ColumnType = "Type"
                case Null
                case Key
                case Default
            }
        }

        guard let wpColumns = try? JSONDecoder().decode([WPColumn].self, from: data) else { return [] }

        return wpColumns.map { col in
            DatabaseColumn(
                name: col.Field,
                type: col.ColumnType,
                isNullable: col.Null == "YES",
                isPrimaryKey: col.Key == "PRI",
                defaultValue: col.Default
            )
        }
    }

    /// Parse row count from query result
    private func parseRowCount(from jsonOutput: String) -> Int {
        guard let data = jsonOutput.data(using: .utf8) else { return 0 }

        // WP-CLI query returns array of row objects
        struct CountResult: Decodable {
            let count: String
        }

        guard let results = try? JSONDecoder().decode([CountResult].self, from: data),
              let first = results.first,
              let count = Int(first.count) else { return 0 }

        return count
    }

    /// Parse rows from query result
    private func parseRows(from jsonOutput: String, columns: [DatabaseColumn]) -> [TableRow] {
        guard let data = jsonOutput.data(using: .utf8) else { return [] }

        // WP-CLI query returns array of dictionaries
        guard let rowDicts = try? JSONDecoder().decode([[String: String?]].self, from: data) else {
            // Try parsing as string values (WP-CLI sometimes returns all strings)
            guard let rowDictsStr = try? JSONDecoder().decode([[String: String]].self, from: data) else {
                return []
            }
            return rowDictsStr.enumerated().map { (index, dict) in
                var values: [String: String?] = [:]
                for (key, value) in dict {
                    values[key] = value == "NULL" ? nil : value
                }
                return TableRow(id: index, values: values)
            }
        }

        return rowDicts.enumerated().map { (index, dict) in
            var values: [String: String?] = [:]
            for (key, value) in dict {
                values[key] = value
            }
            return TableRow(id: index, values: values)
        }
    }
    
    func nextPage() async {
        guard canGoForward else { return }
        currentPage += 1
        await fetchCurrentPage()
    }
    
    func previousPage() async {
        guard canGoBack else { return }
        currentPage -= 1
        await fetchCurrentPage()
    }
    
    func goToPage(_ page: Int) async {
        guard page >= 1 && page <= totalPages else { return }
        currentPage = page
        await fetchCurrentPage()
    }
    
    // MARK: - Refresh
    
    func refresh() async {
        if let table = selectedTable {
            await selectTable(table)
        } else {
            await fetchTables()
        }
    }
    
    // MARK: - Query Mode
    
    func toggleQueryMode() {
        isQueryModeActive.toggle()
    }

    func clearCustomQuery() {
        customQueryText = ""
        customQueryColumns = []
        customQueryRows = []
        customQueryRowCount = 0
        customQueryError = nil
        hasExecutedQuery = false
    }

    func requestRowDeletion(row: TableRow) {
        pendingRowDeletion = PendingRowDeletion(
            table: selectedTable?.name ?? "",
            primaryKeyColumn: columns.first?.name ?? "id",
            primaryKeyValue: row.values.first?.value ?? "",
            rowPreview: row.values.prefix(3).map { "\($0.key): \(String(describing: $0.value))" }.joined(separator: ", ")
        )
        rowDeletionConfirmText = ""
        showRowDeletionConfirm = true
    }

    func confirmRowDeletion() async {
        guard canConfirmRowDeletion,
              let pending = pendingRowDeletion,
              cli.isInstalled else { return }

        isDeletingRow = true

        do {
            let result = try await cli.dbDeleteRow(
                projectId: projectId,
                table: pending.table,
                rowId: pending.primaryKeyValue
            )

            if result.success == true {
                await refresh()
            } else {
                errorMessage = AppError("Delete row failed: \(result.stderr ?? "")", source: "Database Browser")
            }
        } catch {
            errorMessage = AppError("Delete row failed: \(error.localizedDescription)", source: "Database Browser")
        }

        isDeletingRow = false
        showRowDeletionConfirm = false
        pendingRowDeletion = nil
        rowDeletionConfirmText = ""
    }

    func cancelRowDeletion() {
        showRowDeletionConfirm = false
        pendingRowDeletion = nil
        rowDeletionConfirmText = ""
    }

    func runCustomQuery() async {
        let query = customQueryText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            customQueryError = AppError("Enter a query to run", source: "Database Browser")
            return
        }

        guard cli.isInstalled else {
            customQueryError = AppError("Homeboy CLI is not installed", source: "Database Browser")
            return
        }

        isRunningCustomQuery = true
        customQueryError = nil
        customQueryColumns = []
        customQueryRows = []
        selectedQueryRows = []
        customQueryRowCount = 0

        defer {
            isRunningCustomQuery = false
        }

        do {
            let result = try await cli.dbQuery(projectId: projectId, sql: query)

            if result.success == true, let stdout = result.stdout {
                customQueryRows = parseRows(from: stdout, columns: customQueryColumns)
                hasExecutedQuery = true
            } else {
                customQueryError = AppError(result.stderr ?? "Query failed", source: "Database Browser")
            }
        } catch {
            customQueryError = error.toDisplayableError(source: "Database Browser")
        }
    }
    
    // MARK: - Table Deletion

    func requestTableDeletion(_ table: DatabaseTable) {
        pendingTableDeletion = PendingTableDeletion(
            table: table,
            isProtected: false
        )
        tableDeletionConfirmText = ""
        showTableDeletionConfirm = true
    }
    
    func confirmTableDeletion() async {
        guard let pending = pendingTableDeletion,
              !pending.isProtected,
              canConfirmTableDeletion,
              cli.isInstalled else { return }

        isDeletingTable = true

        do {
            let result = try await cli.dbDropTable(projectId: projectId, table: pending.table.name)

            if result.success == true {
                if selectedTable?.name == pending.table.name {
                    selectedTable = nil
                    columns = []
                    rows = []
                }

                await fetchTables()
            } else {
                errorMessage = AppError("Drop table failed: \(result.stderr ?? "")", source: "Database Browser")
            }
        } catch {
            errorMessage = AppError("Drop table failed: \(error.localizedDescription)", source: "Database Browser")
        }

        isDeletingTable = false
        showTableDeletionConfirm = false
        pendingTableDeletion = nil
        tableDeletionConfirmText = ""
    }

    
    func cancelTableDeletion() {
        showTableDeletionConfirm = false
        pendingTableDeletion = nil
        tableDeletionConfirmText = ""
    }
    
    // MARK: - Copy to Clipboard
    
    func copySelectedRows() {
        guard !selectedRows.isEmpty else { return }
        
        let selectedData = rows.filter { selectedRows.contains($0.id) }
        let markdown = formatRowsAsMarkdown(columns: columns, rows: selectedData)
        
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(markdown, forType: .string)
    }
    
    func copySelectedQueryRows() {
        guard !selectedQueryRows.isEmpty else { return }
        
        let selectedData = customQueryRows.filter { selectedQueryRows.contains($0.id) }
        let markdown = formatRowsAsMarkdown(columns: customQueryColumns, rows: selectedData)
        
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(markdown, forType: .string)
    }
    
    private func formatRowsAsMarkdown(columns: [DatabaseColumn], rows: [TableRow]) -> String {
        guard !columns.isEmpty else { return "" }
        
        let columnNames = columns.map { $0.name }
        
        // Header row
        var output = "| " + columnNames.joined(separator: " | ") + " |\n"
        
        // Separator row
        output += "| " + columnNames.map { _ in "---" }.joined(separator: " | ") + " |\n"
        
        // Data rows
        for row in rows {
            let values = columns.map { col in
                let value = row.value(for: col.name)
                // Escape pipe characters in values
                return value.isEmpty ? "NULL" : value.replacingOccurrences(of: "|", with: "\\|")
            }
            output += "| " + values.joined(separator: " | ") + " |\n"
        }
        
        return output
    }

    func retry() async {
        if let table = selectedTable {
            await selectTable(table)
        }
    }

    func disconnect() {
        connectionStatus = .disconnected
        selectedTable = nil
        columns = []
        rows = []
        customQueryRows = []
        tables = []
    }
}
