import Foundation

/// Result of categorizing items into groupings
struct GroupedItems<Item> {
    let grouped: [(grouping: ItemGrouping, items: [Item])]
    let ungrouped: [Item]
    
    var isEmpty: Bool {
        grouped.allSatisfy { $0.items.isEmpty } && ungrouped.isEmpty
    }
    
    var totalItemCount: Int {
        grouped.reduce(0) { $0 + $1.items.count } + ungrouped.count
    }
}

/// Manages item groupings and categorization.
/// Generic operations that work for any item type (tables, components, etc.).
struct GroupingManager {
    
    /// Categorize items into groupings based on membership and pattern matching.
    /// Items not matching any grouping are returned in the ungrouped array.
    /// - Parameters:
    ///   - items: The items to categorize
    ///   - groupings: The groupings to match against
    ///   - idExtractor: Closure to extract the ID from an item for matching
    /// - Returns: Grouped items with their groupings, plus ungrouped remainder
    static func categorize<Item>(
        items: [Item],
        groupings: [ItemGrouping],
        idExtractor: (Item) -> String
    ) -> GroupedItems<Item> {
        var grouped: [(grouping: ItemGrouping, items: [Item])] = []
        var matchedIds = Set<String>()
        
        let sortedGroupings = groupings.sorted { $0.sortOrder < $1.sortOrder }
        
        for grouping in sortedGroupings {
            var groupItems: [Item] = []
            for item in items {
                let itemId = idExtractor(item)
                if !matchedIds.contains(itemId) && grouping.contains(itemId) {
                    groupItems.append(item)
                    matchedIds.insert(itemId)
                }
            }
            grouped.append((grouping: grouping, items: groupItems))
        }
        
        let ungrouped = items.filter { !matchedIds.contains(idExtractor($0)) }
        
        return GroupedItems(grouped: grouped, ungrouped: ungrouped)
    }
    
    /// Create a new grouping from selected item IDs
    static func createGrouping(
        name: String,
        fromIds ids: [String],
        existingGroupings: [ItemGrouping]
    ) -> ItemGrouping {
        let maxSortOrder = existingGroupings.map(\.sortOrder).max() ?? -1
        return ItemGrouping(
            id: UUID().uuidString,
            name: name,
            memberIds: ids,
            patterns: [],
            sortOrder: maxSortOrder + 1
        )
    }
    
    /// Add member IDs to an existing grouping
    static func addMembers(_ ids: [String], to grouping: ItemGrouping) -> ItemGrouping {
        var updated = grouping
        let existingIds = Set(grouping.memberIds)
        let newIds = ids.filter { !existingIds.contains($0) }
        updated.memberIds.append(contentsOf: newIds)
        return updated
    }
    
    /// Remove member IDs from a grouping
    static func removeMembers(_ ids: [String], from grouping: ItemGrouping) -> ItemGrouping {
        var updated = grouping
        let idsToRemove = Set(ids)
        updated.memberIds.removeAll { idsToRemove.contains($0) }
        return updated
    }
    
    /// Reorder groupings after drag/drop, updating sortOrder values
    static func reorder(_ groupings: [ItemGrouping]) -> [ItemGrouping] {
        groupings.enumerated().map { index, grouping in
            var updated = grouping
            updated.sortOrder = index
            return updated
        }
    }
    
    /// Move a grouping from one index to another
    static func moveGrouping(
        in groupings: [ItemGrouping],
        fromIndex: Int,
        toIndex: Int
    ) -> [ItemGrouping] {
        var mutable = groupings.sorted { $0.sortOrder < $1.sortOrder }
        guard fromIndex >= 0, fromIndex < mutable.count,
              toIndex >= 0, toIndex < mutable.count,
              fromIndex != toIndex else {
            return groupings
        }
        let item = mutable.remove(at: fromIndex)
        mutable.insert(item, at: toIndex)
        return reorder(mutable)
    }
    
    /// Update a grouping in a list, returning the updated list
    static func updateGrouping(
        _ grouping: ItemGrouping,
        in groupings: [ItemGrouping]
    ) -> [ItemGrouping] {
        groupings.map { $0.id == grouping.id ? grouping : $0 }
    }
    
    /// Delete a grouping from a list
    static func deleteGrouping(
        id: String,
        from groupings: [ItemGrouping]
    ) -> [ItemGrouping] {
        let filtered = groupings.filter { $0.id != id }
        return reorder(filtered)
    }
}
