import Combine
import Foundation

/// Protocol for ViewModels that need to react to configuration changes.
/// Provides standardized observation of ConfigurationManager.activeProject.
///
/// All conforming ViewModels already have `cancellables` - this protocol
/// reuses that existing property for consistency.
protocol ConfigurationObserving: AnyObject {
    /// Existing cancellables set (all ViewModels already have this)
    var cancellables: Set<AnyCancellable> { get set }

    /// Called when the active project configuration changes.
    /// Implementations should refresh any cached configuration data.
    func onConfigurationChange()
}

extension ConfigurationObserving {
    /// Sets up observation of ConfigurationManager.activeProject changes.
    /// Call this in init() after initial configuration load.
    @MainActor
    func observeConfiguration() {
        ConfigurationManager.shared.$activeProject
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.onConfigurationChange()
            }
            .store(in: &cancellables)
    }
}
