import Foundation

/// Represents an open log tab in the Remote Log Viewer.
struct OpenLog: PinnableTabItem, Equatable {
    let id: UUID
    let path: String
    var isPinned: Bool
    var content: String = ""
    var fileExists: Bool = true
    var lastFetched: Date?
    var tailLines: Int

    var displayName: String {
        URL(fileURLWithPath: path).lastPathComponent
    }

    init(id: UUID = UUID(), path: String, isPinned: Bool, tailLines: Int = 100) {
        self.id = id
        self.path = path
        self.isPinned = isPinned
        self.tailLines = max(1, tailLines)
    }

    init(from pinned: PinnedRemoteLog) {
        self.id = pinned.id
        self.path = pinned.path
        self.isPinned = true
        self.tailLines = max(1, pinned.tailLines)
    }
}
