import Foundation
import LibreSync

struct DemoProfile {
    let name: String
    init(arguments: [String]) throws {
        if arguments.isEmpty { name = "primary"; return }
        guard arguments.count == 2, arguments[0] == "--profile" else {
            throw DemoError.message("Use --profile followed by a profile name.")
        }
        let value = arguments[1]
        guard !value.isEmpty, value.utf8.count <= 32,
              value.utf8.allSatisfy({ (97...122).contains($0) || (48...57).contains($0) || $0 == 45 }) else {
            throw DemoError.message("Profiles use 1–32 lowercase letters, digits, or hyphens.")
        }
        name = value
    }
    var keychainService: String { "io.libresync.NearbySyncDemo.\(name)" }
    func directory() throws -> URL {
        try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            .appendingPathComponent("io.libresync.NearbySyncDemo", isDirectory: true)
            .appendingPathComponent(name, isDirectory: true)
    }
}

enum DemoError: LocalizedError {
    case message(String)
    var errorDescription: String? { if case .message(let text) = self { return text }; return nil }
}

struct DemoNote: Codable, Identifiable, Equatable {
    let id: String
    var title: String
    var body: String
    var updatedAt: Date
}

enum DemoInbox {
    static func notes(in inbox: LibreSyncInbox) throws -> [DemoNote] {
        try inbox.records.filter { $0.adapter == "notes" && !$0.deleted }.map { record in
            let note = try JSONDecoder().decode(DemoNote.self, from: record.value)
            guard note.id == record.id else { throw DemoError.message("A received note has an invalid identity. It has not been acknowledged.") }
            return note
        }.sorted { $0.updatedAt > $1.updatedAt }
    }
    static func apply(_ inbox: LibreSyncInbox, to destination: URL,
                      acknowledge: () async throws -> Void) async throws -> [DemoNote] {
        let notes = try notes(in: inbox)
        // This single durable file is the app's database: values and their exact receipts.
        try await Task.detached {
            try LibreSyncInboxJournal.save(inbox, to: destination)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: destination.path)
            #if os(iOS)
            // Establish plaintext protection before acknowledging this durable inbox.
            try FileManager.default.setAttributes([.protectionKey: FileProtectionType.complete], ofItemAtPath: destination.path)
            #endif
        }.value
        try await acknowledge()
        return notes
    }
}
