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

    // Table groupings (universal system)
    @Published var groupedTables: [(grouping: ItemGrouping, tables: [DatabaseTable], isExpanded: Bool)] = []
    @Published var ungroupedTables: [DatabaseTable] = []
    @Published var isUngroupedExpanded: Bool = true

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
    @Published var customQueryError: String? = nil
    @Published var isRunningCustomQuery: Bool = false
    @Published var customQueryRowCount: Int = 0
    @Published var hasExecutedQuery: Bool = false

    // Error handling
    @Published var errorMessage: AppError?

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

    // Regenerate groupings confirmation
    @Published var showRegenerateGroupingsConfirm: Bool = false

    // Multi-selection for bulk operations
    @Published var selectedTableNames: Set<String> = []

    // MARK: - CLI Bridge
 
    private let cli = HomeboyCLI.shared
 
    private var projectId: String {
        ConfigurationManager.shared.safeActiveProject.id
    }

    // MARK: - Private State

    private var currentGroupings: [ItemGrouping] = []

    // MARK: - Initialization

    init() {
        observeConfiguration()
    }

    // MARK: - Configuration Observation

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectWillSwitch:
            // Reset state on project switch
            disconnect()
        case .projectModified(_, let fields):
            // Only reload groupings if tableGroupings changed
            if fields.contains(.tableGroupings) {
                let project = ConfigurationManager.readCurrentProject()
                currentGroupings = project.tableGroupings
                refreshGroupedTables()
            }
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
        groupedTables.reduce(0) { $0 + $1.tables.count } + ungroupedTables.count
    }

    var canConfirmRowDeletion: Bool {
        rowDeletionConfirmText.lowercased() == "delete"
    }

    var canConfirmTableDeletion: Bool {
        guard let pending = pendingTableDeletion else { return false }
        return tableDeletionConfirmText == pending.table.name
    }

    func isTableProtected(_ table: DatabaseTable) -> Bool {
        let project = ConfigurationManager.readCurrentProject()
        return TableProtectionManager.isProtected(tableName: table.name, config: project)
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
                let allTables = parseTables(from: stdout)

                currentGroupings = ConfigurationManager.readCurrentProject().tableGroupings

                let grouped = currentGroupings.map { grouping in
                    let groupingTables = allTables.filter { table in
                        grouping.memberIds.contains(table.name)
                    }
                    return (grouping: grouping, tables: groupingTables, isExpanded: false)
                }

                let ungrouped = allTables.filter { table in
                    !currentGroupings.contains(where: { $0.memberIds.contains(table.name) })
                }

                groupedTables = grouped
                ungroupedTables = ungrouped
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

    func toggleGroupExpansion(groupingId: String) {
        if let index = groupedTables.firstIndex(where: { $0.grouping.id == groupingId }) {
            groupedTables[index].isExpanded.toggle()
        }
    }

    func toggleUngroupedExpansion() {
        isUngroupedExpanded.toggle()
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
            customQueryError = "Enter a query to run"
            return
        }

        guard cli.isInstalled else {
            customQueryError = "Homeboy CLI is not installed"
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
                customQueryError = result.stderr
            }
        } catch {
            customQueryError = error.localizedDescription
        }
    }
    
    // MARK: - Table Deletion
    
    func requestTableDeletion(_ table: DatabaseTable) {
        let isProtected = isTableProtected(table)
        
        pendingTableDeletion = PendingTableDeletion(
            table: table,
            isProtected: isProtected
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
    
    // MARK: - Grouping Management
    
    /// Create a new grouping from table names
    func createGrouping(name: String, fromTableNames tableNames: [String]) {
        let newGrouping = GroupingManager.createGrouping(
            name: name,
            fromIds: tableNames,
            existingGroupings: currentGroupings
        )
        currentGroupings.append(newGrouping)
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Add tables to an existing grouping
    func addTablesToGrouping(tableNames: [String], groupingId: String) {
        guard let index = currentGroupings.firstIndex(where: { $0.id == groupingId }) else { return }
        currentGroupings[index] = GroupingManager.addMembers(tableNames, to: currentGroupings[index])
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Remove tables from a grouping
    func removeTablesFromGrouping(tableNames: [String], groupingId: String) {
        guard let index = currentGroupings.firstIndex(where: { $0.id == groupingId }) else { return }
        currentGroupings[index] = GroupingManager.removeMembers(tableNames, from: currentGroupings[index])
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Rename a grouping
    func renameGrouping(groupingId: String, newName: String) {
        guard let index = currentGroupings.firstIndex(where: { $0.id == groupingId }) else { return }
        currentGroupings[index].name = newName
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Delete a grouping (tables become ungrouped)
    func deleteGrouping(groupingId: String) {
        currentGroupings = GroupingManager.deleteGrouping(id: groupingId, from: currentGroupings)
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Move a grouping up in the list
    func moveGroupingUp(groupingId: String) {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }),
              index > 0 else { return }
        currentGroupings = GroupingManager.moveGrouping(in: currentGroupings, fromIndex: index, toIndex: index - 1)
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Move a grouping down in the list
    func moveGroupingDown(groupingId: String) {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }),
              index < sorted.count - 1 else { return }
        currentGroupings = GroupingManager.moveGrouping(in: currentGroupings, fromIndex: index, toIndex: index + 1)
        saveGroupings()
        refreshGroupedTables()
    }
    
    /// Check if a grouping can be moved up
    func canMoveGroupingUp(groupingId: String) -> Bool {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }) else { return false }
        return index > 0
    }
    
    /// Check if a grouping can be moved down
    func canMoveGroupingDown(groupingId: String) -> Bool {
        let sorted = currentGroupings.sorted { $0.sortOrder < $1.sortOrder }
        guard let index = sorted.firstIndex(where: { $0.id == groupingId }) else { return false }
        return index < sorted.count - 1
    }
    
    /// Find which grouping a table belongs to (by explicit membership)
    func groupingForTable(_ tableName: String) -> ItemGrouping? {
        currentGroupings.first { $0.memberIds.contains(tableName) }
    }
    
    /// Check if a table is in a group by explicit membership (not pattern)
    func isTableInGroupByMembership(_ tableName: String) -> Bool {
        currentGroupings.contains { $0.memberIds.contains(tableName) }
    }

    // MARK: - Regenerate Default Groupings

    /// Request regeneration of default groupings (shows confirmation)
    func requestRegenerateDefaultGroupings() {
        showRegenerateGroupingsConfirm = true
    }

    /// Regenerate default groupings from project type definition (after confirmation)
    func confirmRegenerateDefaultGroupings() {
        let project = ConfigurationManager.readCurrentProject()
        let groupings = SchemaResolver.resolveDefaultGroupings(for: project)
        currentGroupings = groupings
        saveGroupings()
        refreshGroupedTables()
        showRegenerateGroupingsConfirm = false
    }

    /// Cancel regenerate groupings confirmation
    func cancelRegenerateDefaultGroupings() {
        showRegenerateGroupingsConfirm = false
    }

    // MARK: - Multi-Selection Management

    /// Toggle a table's selection state for bulk operations
    func toggleTableSelection(_ tableName: String) {
        if selectedTableNames.contains(tableName) {
            selectedTableNames.remove(tableName)
        } else {
            selectedTableNames.insert(tableName)
        }
    }

    /// Clear all table selections
    func clearTableSelection() {
        selectedTableNames.removeAll()
    }

    /// Select all tables in a specific group
    func selectAllTablesInGroup(groupingId: String) {
        if let group = groupedTables.first(where: { $0.grouping.id == groupingId }) {
            for table in group.tables {
                selectedTableNames.insert(table.name)
            }
        }
    }

    /// Select all ungrouped tables
    func selectAllUngroupedTables() {
        selectedRows = []
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
    }

    /// Add all selected tables to a grouping
    func addSelectedTablesToGrouping(groupingId: String) {
        guard !selectedTableNames.isEmpty else { return }
        addTablesToGrouping(tableNames: Array(selectedTableNames), groupingId: groupingId)
        selectedTableNames.removeAll()
    }

    /// Protect all selected tables
    func protectSelectedTables() {
        let tablesToProtect = selectedTableNames
        ConfigurationManager.shared.updateActiveProject { project in
            for tableName in tablesToProtect {
                TableProtectionManager.protect(tableName: tableName, in: &project)
            }
        }
        selectedTableNames.removeAll()
    }

    /// Unprotect all selected tables
    func unprotectSelectedTables() {
        let tablesToUnprotect = selectedTableNames
        ConfigurationManager.shared.updateActiveProject { project in
            for tableName in tablesToUnprotect {
                TableProtectionManager.unprotect(tableName: tableName, in: &project)
            }
        }
        selectedTableNames.removeAll()
    }
    
    // MARK: - Table Protection Management

    /// Protect a table from deletion
    func protectTable(_ tableName: String) {
        ConfigurationManager.shared.updateActiveProject { project in
            TableProtectionManager.protect(tableName: tableName, in: &project)
        }
    }

    /// Remove protection from a table
    func unprotectTable(_ tableName: String) {
        ConfigurationManager.shared.updateActiveProject { project in
            TableProtectionManager.unprotect(tableName: tableName, in: &project)
        }
    }

    /// Unlock a core protected table (allows deletion)
    func unlockTable(_ tableName: String) {
        ConfigurationManager.shared.updateActiveProject { project in
            TableProtectionManager.unlock(tableName: tableName, in: &project)
        }
    }
    
    /// Check if a table is a core protected table
    func isCoreProtectedTable(_ tableName: String) -> Bool {
        let project = ConfigurationManager.readCurrentProject()
        return TableProtectionManager.isCoreProtected(tableName: tableName, config: project)
    }
    
    /// Check if a table has been unlocked
    func isTableUnlocked(_ tableName: String) -> Bool {
        let project = ConfigurationManager.readCurrentProject()
        return TableProtectionManager.isUnlocked(tableName: tableName, config: project)
    }
    
    // MARK: - Private Helpers
    
    private func saveGroupings() {
        let groupings = currentGroupings
        ConfigurationManager.shared.updateActiveProject { project in
            project.tableGroupings = groupings
        }
    }
    
    private func refreshGroupedTables() {
        let allTables = groupedTables.flatMap { $0.tables } + ungroupedTables
        let result = GroupingManager.categorize(
            items: allTables,
            groupings: currentGroupings,
            idExtractor: { $0.name }
        )
        
        // Preserve expansion state where possible
        let oldExpansionState = Dictionary(uniqueKeysWithValues: groupedTables.map { ($0.grouping.id, $0.isExpanded) })
        
        groupedTables = result.grouped.map { (grouping, items) in
            let wasExpanded = oldExpansionState[grouping.id] ?? false
            return (grouping: grouping, tables: items, isExpanded: wasExpanded)
        }
        ungroupedTables = result.ungrouped
    }
}
