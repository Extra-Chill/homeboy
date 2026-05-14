import Foundation

/// Represents an open file tab in the Remote File Editor.
struct OpenFile: PinnableTabItem, Equatable {
    let id: UUID
    let path: String
    var isPinned: Bool
    var content: String = ""
    var originalContent: String = ""
    var fileExists: Bool = true
    var lastFetched: Date?
    var fileSize: Int64?

    var displayName: String {
        URL(fileURLWithPath: path).lastPathComponent
    }

    var hasUnsavedChanges: Bool {
        content != originalContent
    }

    var formattedSize: String {
        guard let fileSize else { return "" }
        return ByteCountFormatter.string(fromByteCount: fileSize, countStyle: .file)
    }

    init(id: UUID = UUID(), path: String, isPinned: Bool) {
        self.id = id
        self.path = path
        self.isPinned = isPinned
    }

    init(from pinned: PinnedRemoteFile) {
        self.id = pinned.id
        self.path = pinned.path
        self.isPinned = true
    }
}
