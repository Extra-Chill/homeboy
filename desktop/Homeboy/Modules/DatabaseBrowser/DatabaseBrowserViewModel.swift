import AppKit
import Combine
import Foundation
import SwiftUI

/// ViewModel for the Database Browser module
@MainActor
class DatabaseBrowserViewModel: ObservableObject {
    
    private var cancellables = Set<AnyCancellable>()
    
    // MARK: - Published State
    
    @Published var connectionStatus: DatabaseConnectionStatus = .disconnected
    
    // Site-based organization
    @Published var sites: [WordPressSite] = []
    @Published var networkCategory: TableCategory = TableCategory(name: "Network")
    @Published var otherCategory: TableCategory = TableCategory(name: "Other")
    
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
    @Published var errorMessage: String?
    
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
    
    // Detected prefix for protection system
    @Published var detectedPrefix: String? = nil
    
    // MARK: - Private State
    
    private var mysqlService: MySQLService?
    
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
        SSHService.isConfigured() && KeychainService.hasLiveMySQLCredentials()
    }
    
    var totalTableCount: Int {
        sites.reduce(0) { $0 + $1.tableCount } + networkCategory.tableCount + otherCategory.tableCount
    }
    
    var canConfirmRowDeletion: Bool {
        rowDeletionConfirmText.lowercased() == "delete"
    }
    
    var canConfirmTableDeletion: Bool {
        guard let pending = pendingTableDeletion else { return false }
        return tableDeletionConfirmText == pending.table.name
    }
    
    func isTableProtected(_ table: DatabaseTable) -> Bool {
        WordPressSiteMap.isProtectedTable(table.name, prefix: detectedPrefix)
    }
    
    // MARK: - Connection Methods
    
    func connect() async {
        guard isConfigured else {
            connectionStatus = .error("Database credentials not configured. Check Settings.")
            return
        }
        
        connectionStatus = .connecting
        errorMessage = nil
        
        do {
            mysqlService = try MySQLService()
            try await mysqlService?.connect()
            connectionStatus = .connected
            
            // Fetch tables after connecting
            await fetchTables()
        } catch {
            connectionStatus = .error(error.localizedDescription)
            errorMessage = error.localizedDescription
            mysqlService = nil
        }
    }
    
    func disconnect() {
        mysqlService?.disconnect()
        mysqlService = nil
        connectionStatus = .disconnected
        
        // Clear state
        sites = []
        networkCategory = TableCategory(name: "Network")
        otherCategory = TableCategory(name: "Other")
        selectedTable = nil
        columns = []
        rows = []
        currentPage = 1
        totalRows = 0
        errorMessage = nil
    }
    
    // MARK: - Site Expansion
    
    func toggleSiteExpansion(_ site: WordPressSite) {
        if let index = sites.firstIndex(where: { $0.id == site.id }) {
            sites[index].isExpanded.toggle()
        }
    }
    
    func toggleNetworkExpansion() {
        networkCategory.isExpanded.toggle()
    }
    
    func toggleOtherExpansion() {
        otherCategory.isExpanded.toggle()
    }
    
    // MARK: - Data Methods
    
    private func fetchTables() async {
        guard let service = mysqlService else { return }
        
        do {
            let allTables = try await service.fetchTables()
            
            // Detect table prefix for protection system
            detectedPrefix = WordPressSiteMap.detectTablePrefix(from: allTables)
            
            // Get multisite config from current site configuration
            let siteConfig = ConfigurationManager.readCurrentProject()
            let (categorizedSites, network, other) = WordPressSiteMap.categorize(
                tables: allTables,
                config: siteConfig.multisite
            )
            
            sites = categorizedSites
            networkCategory = TableCategory(name: "Network", tables: network)
            otherCategory = TableCategory(name: "Other", tables: other)
        } catch {
            errorMessage = "Failed to fetch tables: \(error.localizedDescription)"
        }
    }
    
    func selectTable(_ table: DatabaseTable) async {
        isQueryModeActive = false
        selectedTable = table
        currentPage = 1
        rows = []
        columns = []
        totalRows = 0
        selectedRows = []
        isLoadingTableData = true
        
        guard let service = mysqlService else {
            isLoadingTableData = false
            return
        }
        
        do {
            columns = try await service.fetchColumns(table: table.name)
            totalRows = try await service.getRowCount(table: table.name)
            rows = try await service.fetchRowsWithColumns(
                table: table.name,
                columns: columns,
                limit: rowsPerPage,
                offset: 0
            )
        } catch {
            errorMessage = "Failed to load table: \(error.localizedDescription)"
        }
        
        isLoadingTableData = false
    }
    
    func fetchCurrentPage() async {
        guard let service = mysqlService,
              let table = selectedTable else { return }
        
        let offset = (currentPage - 1) * rowsPerPage
        
        do {
            rows = try await service.fetchRowsWithColumns(
                table: table.name,
                columns: columns,
                limit: rowsPerPage,
                offset: offset
            )
        } catch {
            errorMessage = "Failed to fetch rows: \(error.localizedDescription)"
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
    
    func runCustomQuery() async {
        let query = customQueryText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            customQueryError = "Enter a query to run"
            return
        }
        
        guard let service = mysqlService else {
            customQueryError = "Not connected to database"
            return
        }
        
        isRunningCustomQuery = true
        customQueryError = nil
        customQueryColumns = []
        customQueryRows = []
        selectedQueryRows = []
        customQueryRowCount = 0
        
        do {
            print("[Query Debug] Executing query: \(query.prefix(100))")
            let (columnNames, rowData) = try await service.executeCustomQuery(query)
            print("[Query Debug] Received \(columnNames.count) columns, \(rowData.count) rows")
            
            // Convert column names to DatabaseColumn objects
            customQueryColumns = columnNames.map { name in
                DatabaseColumn(
                    name: name,
                    type: "",
                    isNullable: true,
                    isPrimaryKey: false,
                    defaultValue: nil
                )
            }
            
            // Convert row data to TableRow objects
            customQueryRows = rowData.enumerated().map { (index, values) in
                var rowValues: [String: String?] = [:]
                for (colIndex, column) in customQueryColumns.enumerated() {
                    if colIndex < values.count {
                        let value = values[colIndex]
                        rowValues[column.name] = value == "NULL" ? nil : value
                    }
                }
                return TableRow(id: index, values: rowValues)
            }
            
            customQueryRowCount = customQueryRows.count
        } catch {
            customQueryError = error.localizedDescription
        }
        
        hasExecutedQuery = true
        isRunningCustomQuery = false
    }
    
    func clearCustomQuery() {
        customQueryText = ""
        customQueryColumns = []
        customQueryRows = []
        selectedQueryRows = []
        customQueryError = nil
        customQueryRowCount = 0
        hasExecutedQuery = false
    }
    
    // MARK: - Row Deletion
    
    func requestRowDeletion(row: TableRow) {
        guard let table = selectedTable,
              let primaryKey = columns.first(where: { $0.isPrimaryKey }) else {
            errorMessage = "Cannot delete: no primary key found"
            return
        }
        
        guard let pkValue = row.values[primaryKey.name], pkValue != nil else {
            errorMessage = "Cannot delete: primary key value is null"
            return
        }
        
        // Build row preview from first 3 non-null values
        let preview = columns.prefix(3)
            .compactMap { col -> String? in
                if let val = row.values[col.name], let unwrapped = val {
                    return unwrapped
                }
                return nil
            }
            .joined(separator: ", ")
        
        pendingRowDeletion = PendingRowDeletion(
            table: table.name,
            primaryKeyColumn: primaryKey.name,
            primaryKeyValue: pkValue!,
            rowPreview: preview
        )
        rowDeletionConfirmText = ""
        showRowDeletionConfirm = true
    }
    
    func confirmRowDeletion() async {
        guard let pending = pendingRowDeletion,
              canConfirmRowDeletion,
              let service = mysqlService else { return }
        
        isDeletingRow = true
        
        do {
            try await service.deleteRow(
                table: pending.table,
                primaryKeyColumn: pending.primaryKeyColumn,
                primaryKeyValue: pending.primaryKeyValue
            )
            
            // Refresh current view
            await refresh()
        } catch {
            errorMessage = "Delete failed: \(error.localizedDescription)"
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
              let service = mysqlService else { return }
        
        isDeletingTable = true
        
        do {
            try await service.dropTable(table: pending.table.name)
            
            // Clear selection if we dropped the selected table
            if selectedTable?.name == pending.table.name {
                selectedTable = nil
                columns = []
                rows = []
            }
            
            // Refresh table list
            await fetchTables()
        } catch {
            errorMessage = "Drop table failed: \(error.localizedDescription)"
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
    
    // MARK: - Site Switching
    
    func setupSiteChangeObserver() {
        NotificationCenter.default.publisher(for: .projectWillChange)
            .sink { [weak self] _ in
                self?.disconnect()
            }
            .store(in: &cancellables)
    }
}
