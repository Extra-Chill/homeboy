import Combine
import Foundation
import SwiftUI

@MainActor
final class QualityViewModel: ObservableObject, ConfigurationObserving {
    var cancellables = Set<AnyCancellable>()

    @Published var components: [ComponentConfiguration] = []
    @Published var selectedComponentId: String?
    @Published var scope: QualityScope = .full
    @Published var changedSince = "origin/main"
    @Published var review: QualityReviewOutput?
    @Published var triage: QualityTriageOutput?
    @Published var consoleOutput = ""
    @Published var isRunningReview = false
    @Published var isRunningTriage = false
    @Published var runningStage: QualityStage?
    @Published var error: (any DisplayableError)?

    private let configManager = ConfigurationManager.shared

    init() {
        observeConfiguration()
        refreshComponents()
    }

    var selectedComponent: ComponentConfiguration? {
        guard let selectedComponentId else { return components.first }
        return components.first { $0.id == selectedComponentId }
    }

    var canRun: Bool {
        selectedComponent != nil && !isBusy
    }

    var isBusy: Bool {
        isRunningReview || isRunningTriage || runningStage != nil
    }

    func handleConfigChange(_ change: ConfigurationChangeType) {
        switch change {
        case .projectDidSwitch, .projectModified:
            refreshComponents()
        default:
            break
        }
    }

    func refreshComponents() {
        if let project = configManager.activeProject {
            components = configManager.loadComponentsForProject(project)
        } else {
            components = configManager.loadAllComponents()
        }

        if selectedComponentId == nil || !components.contains(where: { $0.id == selectedComponentId }) {
            selectedComponentId = components.first?.id
        }
    }

    func runReview() {
        guard let component = selectedComponent else { return }
        isRunningReview = true
        error = nil
        consoleOutput = "Running homeboy review --summary..."

        Task {
            do {
                review = try await HomeboyCLI.shared.qualityReviewSummary(
                    componentId: component.id,
                    path: component.localPath,
                    scope: scope,
                    changedSince: changedSinceValue
                )
                consoleOutput = "Review summary loaded for \(component.id)."
            } catch {
                self.error = error.toDisplayableError(source: "Quality Review")
                consoleOutput = error.localizedDescription
            }
            isRunningReview = false
        }
    }

    func runTriage() {
        guard let component = selectedComponent else { return }
        isRunningTriage = true
        error = nil
        consoleOutput = "Running homeboy triage component..."

        Task {
            do {
                triage = try await HomeboyCLI.shared.qualityTriage(componentId: component.id)
                consoleOutput = "Triage report loaded for \(component.id)."
            } catch {
                self.error = error.toDisplayableError(source: "Quality Triage")
                consoleOutput = error.localizedDescription
            }
            isRunningTriage = false
        }
    }

    func runStage(_ stage: QualityStage) {
        guard let component = selectedComponent else { return }
        runningStage = stage
        error = nil
        consoleOutput = "Running homeboy \(stage.rawValue)..."

        Task {
            do {
                let response = try await HomeboyCLI.shared.qualityStage(
                    stage,
                    componentId: component.id,
                    path: component.localPath,
                    scope: scope,
                    changedSince: changedSinceValue
                )
                consoleOutput = response.output.isEmpty ? response.errorOutput : response.output
            } catch {
                self.error = error.toDisplayableError(source: "Quality \(stage.label)")
                consoleOutput = error.localizedDescription
            }
            runningStage = nil
        }
    }

    private var changedSinceValue: String? {
        let value = changedSince.trimmingCharacters(in: .whitespacesAndNewlines)
        return value.isEmpty ? nil : value
    }
}
