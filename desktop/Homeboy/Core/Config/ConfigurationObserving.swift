import Combine
import Foundation

/// Protocol for ViewModels that need to react to configuration changes.
/// Uses ConfigurationObserver for typed change events across all config files.
@MainActor
protocol ConfigurationObserving: AnyObject {
    /// Existing cancellables set (all ViewModels already have this)
    var cancellables: Set<AnyCancellable> { get set }

    /// Handle a typed configuration change.
    /// Implementations choose which changes to react to via switch statement.
    func handleConfigChange(_ change: ConfigurationChangeType)
}

extension ConfigurationObserving {
    /// Sets up observation of all configuration changes via ConfigurationObserver.
    /// Call this in init() after initial configuration load.
    func observeConfiguration() {
        ConfigurationObserver.shared.$lastChange
            .compactMap { $0 }
            .receive(on: DispatchQueue.main)
            .sink { [weak self] change in
                self?.handleConfigChange(change)
            }
            .store(in: &cancellables)
    }
}
