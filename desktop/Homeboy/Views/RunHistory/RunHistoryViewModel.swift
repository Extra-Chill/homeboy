import Foundation

@MainActor
final class RunHistoryViewModel: ObservableObject {
    @Published var runs: [RunSummary] = []
    @Published var selectedRunID: String?
    @Published var selectedRunDetail: RunDetail?
    @Published var findings: [RunFinding] = []
    @Published var kindFilter = ""
    @Published var statusFilter = ""
    @Published var componentFilter = ""
    @Published var rigFilter = ""
    @Published var limit = 50
    @Published var isLoadingRuns = false
    @Published var isLoadingDetail = false
    @Published var error: (any DisplayableError)?

    private var loadedDetailRunID: String?

    var hasActiveFilters: Bool {
        !kindFilter.isEmpty || !statusFilter.isEmpty || !componentFilter.isEmpty || !rigFilter.isEmpty || limit != 50
    }

    var selectedRun: RunSummary? {
        runs.first { $0.id == selectedRunID }
    }

    func loadRuns() {
        guard HomeboyCLI.shared.isInstalled else {
            error = AppError("Homeboy CLI is not installed. Install via Settings -> CLI.", source: "Run History")
            return
        }

        isLoadingRuns = true
        error = nil

        Task {
            do {
                let loaded = try await HomeboyCLI.shared.runsList(filter: currentFilter)
                runs = loaded
                isLoadingRuns = false

                if let selectedRunID, loaded.contains(where: { $0.id == selectedRunID }) {
                    loadSelectedRunIfNeeded()
                } else {
                    selectRun(loaded.first?.id)
                }
            } catch {
                isLoadingRuns = false
                self.error = error.toDisplayableError(source: "Run History")
            }
        }
    }

    func selectRun(_ id: String?) {
        guard selectedRunID != id else { return }
        selectedRunID = id
        selectedRunDetail = nil
        findings = []
        loadedDetailRunID = nil
        loadSelectedRunIfNeeded()
    }

    func applyFilters() {
        selectedRunID = nil
        selectedRunDetail = nil
        findings = []
        loadedDetailRunID = nil
        loadRuns()
    }

    func resetFilters() {
        kindFilter = ""
        statusFilter = ""
        componentFilter = ""
        rigFilter = ""
        limit = 50
        applyFilters()
    }

    func loadSelectedRunIfNeeded() {
        guard let id = selectedRunID, loadedDetailRunID != id, !isLoadingDetail else { return }
        isLoadingDetail = true
        error = nil

        Task {
            do {
                async let detailTask = HomeboyCLI.shared.runsShow(id: id)
                async let findingsTask = HomeboyCLI.shared.runsFindings(id: id, limit: 100)
                selectedRunDetail = try await detailTask
                findings = try await findingsTask
                loadedDetailRunID = id
            } catch {
                self.error = error.toDisplayableError(source: "Run History")
            }

            isLoadingDetail = false
        }
    }

    private var currentFilter: RunsListFilter {
        RunsListFilter(
            kind: nilIfEmpty(kindFilter),
            componentId: nilIfEmpty(componentFilter),
            rigId: nilIfEmpty(rigFilter),
            status: nilIfEmpty(statusFilter),
            limit: limit
        )
    }

    private func nilIfEmpty(_ value: String) -> String? {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
