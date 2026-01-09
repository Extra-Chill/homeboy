import AppKit
import Foundation

@MainActor
struct DeployerComponentMenuContext {
    let selectedIds: Set<String>
    let allGroupings: [ItemGrouping]

    let onCreateGroupFromSelection: (Set<String>) -> Void
    let onAddSelectionToGroup: (Set<String>, ItemGrouping) -> Void
    let onRemoveSelectionFromGroup: (Set<String>) -> Void
    let onDeploySelected: (Set<String>) -> Void
    let onRefreshVersions: () -> Void
}

enum DeployerComponentGroupingContext {
    case grouped(grouping: ItemGrouping)
    case ungrouped
}

private final class DeployerClosureMenuItem: NSMenuItem {
    private let actionClosure: () -> Void

    init(title: String, action: @escaping () -> Void) {
        self.actionClosure = action
        super.init(title: title, action: #selector(performAction), keyEquivalent: "")
        target = self
    }

    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    @objc private func performAction() {
        actionClosure()
    }
}

@MainActor
func makeDeployerComponentContextMenu(
    selectedIds: Set<String>,
    groupingContext: DeployerComponentGroupingContext,
    allGroupings: [ItemGrouping],
    onCreateGroupFromSelection: @escaping (Set<String>) -> Void,
    onAddSelectionToGroup: @escaping (Set<String>, ItemGrouping) -> Void,
    onRemoveSelectionFromGroup: @escaping (Set<String>) -> Void,
    onDeploySelected: @escaping (Set<String>) -> Void,
    onRefreshVersions: @escaping () -> Void
) -> NSMenu? {
    guard !selectedIds.isEmpty else { return nil }

    let context = DeployerComponentMenuContext(
        selectedIds: selectedIds,
        allGroupings: allGroupings,
        onCreateGroupFromSelection: onCreateGroupFromSelection,
        onAddSelectionToGroup: onAddSelectionToGroup,
        onRemoveSelectionFromGroup: onRemoveSelectionFromGroup,
        onDeploySelected: onDeploySelected,
        onRefreshVersions: onRefreshVersions
    )

    let menu = NSMenu()

    let addToGroupMenu = NSMenu()
    for grouping in allGroupings {
        addToGroupMenu.addItem(DeployerClosureMenuItem(title: grouping.name) {
            context.onAddSelectionToGroup(context.selectedIds, grouping)
        })
    }

    let addToGroupItem = NSMenuItem(title: "Add to Group…", action: nil, keyEquivalent: "")
    addToGroupItem.submenu = addToGroupMenu
    addToGroupItem.isEnabled = !allGroupings.isEmpty
    menu.addItem(addToGroupItem)

    menu.addItem(DeployerClosureMenuItem(title: "New Group…") {
        context.onCreateGroupFromSelection(context.selectedIds)
    })

    switch groupingContext {
    case .grouped:
        menu.addItem(DeployerClosureMenuItem(title: "Remove from Group") {
            context.onRemoveSelectionFromGroup(context.selectedIds)
        })
    case .ungrouped:
        break
    }

    menu.addItem(.separator())

    menu.addItem(DeployerClosureMenuItem(title: "Deploy Selected") {
        context.onDeploySelected(context.selectedIds)
    })

    menu.addItem(DeployerClosureMenuItem(title: "Refresh Versions") {
        context.onRefreshVersions()
    })

    return menu
}
