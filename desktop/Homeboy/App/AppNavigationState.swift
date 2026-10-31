import Foundation

/// Holds the sidebar selection so any detail view (e.g. Activity) can drive
/// navigation without ContentView passing bindings through every layer.
@MainActor
final class AppNavigationState: ObservableObject {
    @Published var selectedItem: NavigationItem? = .activity

    /// A run Activity asked Missions to select once it becomes visible.
    /// `MissionsView` consumes (and clears) this on activation rather than
    /// Activity holding a reference to `MissionStore` itself.
    @Published private(set) var pendingRunSelection: String?

    /// Switches to the Missions workspace and hands off a run to select
    /// there. Called from Activity when the user picks a run.
    func openRun(_ runId: String) {
        pendingRunSelection = runId
        selectedItem = .missions
    }

    /// Consumes the pending run selection, if any, clearing it so it is not
    /// re-applied the next time Missions becomes visible.
    func consumePendingRunSelection() -> String? {
        defer { pendingRunSelection = nil }
        return pendingRunSelection
    }
}
