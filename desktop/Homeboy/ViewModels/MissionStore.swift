import Foundation

/// Global orchestration store backing Activity and Missions. Pages missions
/// and runs from `ControlPlaneClient`, keeps runs grouped by mission, and
/// polls the selected run's event stream by cursor while it is non-terminal.
///
/// Homeboy core remains the single source of truth: this store only mirrors
/// what core returns and never derives state locally.
@MainActor
final class MissionStore: ObservableObject {
    @Published private(set) var missions: [ControlPlaneMission] = []
    @Published private(set) var missionsHasMore = false
    @Published private(set) var isLoadingMissions = false

    /// Runs keyed by mission id, newest first within each mission.
    @Published private(set) var runsByMission: [String: [ControlPlaneRun]] = [:]
    /// All known runs keyed by run id, regardless of mission grouping.
    @Published private(set) var runsById: [String: ControlPlaneRun] = [:]
    @Published private(set) var runsHasMore = false
    @Published private(set) var isLoadingRuns = false

    @Published var selectedRunId: String?
    @Published private(set) var selectedRunEvents: [ControlPlaneEvent] = []
    @Published private(set) var selectedRunReview: ControlPlaneRunReview?
    @Published private(set) var isLoadingEvents = false
    @Published private(set) var isLoadingReview = false

    @Published var error: (any DisplayableError)?

    private let client: ControlPlaneClient
    private var missionsCursor: String?
    private var runsCursor: String?
    private var eventsCursor: String?
    private var pollingTask: Task<Void, Never>?

    init(client: ControlPlaneClient = .shared) {
        self.client = client
    }

    deinit {
        pollingTask?.cancel()
    }

    var selectedRun: ControlPlaneRun? {
        selectedRunId.flatMap { runsById[$0] }
    }

    /// Runs across every mission, newest-created first. Used by Activity.
    var allRunsNewestFirst: [ControlPlaneRun] {
        runsById.values.sorted { lhs, rhs in
            (ControlPlaneDate.parse(lhs.createdAt) ?? .distantPast)
                > (ControlPlaneDate.parse(rhs.createdAt) ?? .distantPast)
        }
    }

    // MARK: - Missions

    func loadMissions(reset: Bool = true) async {
        if reset {
            missionsCursor = nil
            missions = []
        }
        guard !isLoadingMissions else { return }
        isLoadingMissions = true
        error = nil
        do {
            let page = try await client.missions(limit: 50, cursor: missionsCursor)
            if reset {
                missions = page.missions
            } else {
                missions.append(contentsOf: page.missions)
            }
            missionsCursor = page.nextCursor
            missionsHasMore = page.hasMore
        } catch {
            self.error = error.toDisplayableError(source: "Missions")
        }
        isLoadingMissions = false
    }

    func loadMoreMissionsIfNeeded() async {
        guard missionsHasMore, !isLoadingMissions else { return }
        await loadMissions(reset: false)
    }

    // MARK: - Runs

    func loadRuns(mission: String? = nil, reset: Bool = true) async {
        if reset {
            runsCursor = nil
        }
        guard !isLoadingRuns else { return }
        isLoadingRuns = true
        error = nil
        do {
            let page = try await client.runs(limit: 50, cursor: runsCursor, mission: mission)
            store(runs: page.runs, replaceMission: reset ? mission : nil)
            runsCursor = page.nextCursor
            runsHasMore = page.hasMore
        } catch {
            self.error = error.toDisplayableError(source: "Missions")
        }
        isLoadingRuns = false
    }

    func loadMoreRunsIfNeeded(mission: String? = nil) async {
        guard runsHasMore, !isLoadingRuns else { return }
        await loadRuns(mission: mission, reset: false)
    }

    func runs(forMission mission: String) -> [ControlPlaneRun] {
        runsByMission[mission] ?? []
    }

    private func store(runs: [ControlPlaneRun], replaceMission: String?) {
        for run in runs {
            runsById[run.run] = run
        }

        if let replaceMission {
            let existing = runsByMission[replaceMission] ?? []
            let incomingIds = Set(runs.map(\.run))
            runsByMission[replaceMission] = runs + existing.filter { !incomingIds.contains($0.run) }
        } else {
            for run in runs {
                guard let mission = run.mission else { continue }
                var bucket = runsByMission[mission] ?? []
                if let index = bucket.firstIndex(where: { $0.run == run.run }) {
                    bucket[index] = run
                } else {
                    bucket.append(run)
                }
                runsByMission[mission] = bucket
            }
        }
    }

    /// Refreshes one run in place (e.g. after an action acknowledgement).
    func applyUpdatedRun(_ run: ControlPlaneRun) {
        runsById[run.run] = run
        if let mission = run.mission {
            var bucket = runsByMission[mission] ?? []
            if let index = bucket.firstIndex(where: { $0.run == run.run }) {
                bucket[index] = run
            } else {
                bucket.insert(run, at: 0)
            }
            runsByMission[mission] = bucket
        }
    }

    // MARK: - Selected run

    func selectRun(_ runId: String?) {
        guard selectedRunId != runId else { return }
        stopPolling()
        selectedRunId = runId
        selectedRunEvents = []
        selectedRunReview = nil
        eventsCursor = nil

        guard let runId else { return }

        Task {
            await refreshSelectedRun(runId: runId)
            await loadReview(runId: runId)
            startPollingIfNeeded(runId: runId)
        }
    }

    func refreshSelectedRun() async {
        guard let runId = selectedRunId else { return }
        await refreshSelectedRun(runId: runId)
    }

    private func refreshSelectedRun(runId: String) async {
        do {
            let run = try await client.run(id: runId)
            applyUpdatedRun(run)
        } catch {
            self.error = error.toDisplayableError(source: "Missions")
        }
    }

    func loadReview(runId: String? = nil) async {
        let runId = runId ?? selectedRunId
        guard let runId else { return }
        isLoadingReview = true
        do {
            selectedRunReview = try await client.review(runId: runId)
        } catch {
            self.error = error.toDisplayableError(source: "Missions")
        }
        isLoadingReview = false
    }

    /// Called when the mission detail view disappears so polling stops
    /// spending daemon requests for a run nobody is looking at.
    func stopPolling() {
        pollingTask?.cancel()
        pollingTask = nil
    }

    /// Called when the mission detail view reappears so polling for the
    /// still-selected run resumes without re-fetching the run and events
    /// from scratch (which `selectRun` would do for a fresh selection).
    func resumePollingIfNeeded() {
        guard let runId = selectedRunId else { return }
        startPollingIfNeeded(runId: runId)
    }

    private func startPollingIfNeeded(runId: String) {
        guard selectedRunId == runId else { return }
        guard let run = runsById[runId], !run.isTerminal else { return }

        pollingTask?.cancel()
        pollingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                guard !Task.isCancelled, let self else { return }
                let isTerminal = await self.pollEvents(runId: runId)
                if isTerminal { return }
            }
        }
    }

    /// Fetches the next page of events by cursor, appends new sequences
    /// without duplicating any already held, and re-snapshots from the start
    /// when the daemon reports the cursor has expired. Returns `true` when
    /// the run is now terminal and polling should stop.
    @discardableResult
    private func pollEvents(runId: String) async -> Bool {
        guard selectedRunId == runId else { return true }
        isLoadingEvents = true
        defer { isLoadingEvents = false }

        do {
            let page = try await client.events(runId: runId, cursor: eventsCursor)
            appendEvents(page.events)
            eventsCursor = page.nextCursor
        } catch let controlPlaneError as ControlPlaneError where controlPlaneError.isCursorExpired {
            selectedRunEvents = []
            eventsCursor = nil
            await snapshotEvents(runId: runId)
        } catch {
            self.error = error.toDisplayableError(source: "Missions")
            return true
        }

        await refreshSelectedRun(runId: runId)
        return runsById[runId]?.isTerminal ?? true
    }

    private func snapshotEvents(runId: String) async {
        do {
            let page = try await client.events(runId: runId, cursor: nil)
            selectedRunEvents = []
            appendEvents(page.events)
            eventsCursor = page.nextCursor
        } catch {
            self.error = error.toDisplayableError(source: "Missions")
        }
    }

    private func appendEvents(_ events: [ControlPlaneEvent]) {
        let existingIds = Set(selectedRunEvents.map(\.event))
        let newEvents = events.filter { !existingIds.contains($0.event) }
        selectedRunEvents.append(contentsOf: newEvents)
        selectedRunEvents.sort { $0.sequence < $1.sequence }
    }
}
