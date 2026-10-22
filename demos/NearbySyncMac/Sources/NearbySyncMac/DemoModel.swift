import AppKit
import SwiftUI
import LibreSync

@MainActor
final class DemoModel: ObservableObject {
    @Published var sync: LibreSyncSessionModel?
    @Published var notes: [DemoNote] = []
    @Published var error: String?
    @Published var busy = false
    let profile: DemoProfile?
    private var poller: Task<Void, Never>?
    private var operation: Task<Void, Never>?
    private var startup: Task<Void, Never>?
    private let stateDirectory: (DemoProfile) throws -> URL
    private let openSession: (LibreSyncSessionConfiguration, any LibreSyncSecureStore) async throws -> LibreSyncSession
    private var lastInbox: Data?
    private var closing = false

    init(arguments: [String] = Array(CommandLine.arguments.dropFirst()),
         stateDirectory: @escaping (DemoProfile) throws -> URL = { try $0.directory() },
         openSession: @escaping (LibreSyncSessionConfiguration, any LibreSyncSecureStore) async throws -> LibreSyncSession = { try await LibreSyncSession.open($0, keys: $1) }) {
        self.stateDirectory = stateDirectory
        self.openSession = openSession
        do { profile = try DemoProfile(arguments: arguments) }
        catch { profile = nil; self.error = error.localizedDescription }
    }

    func start() async {
        guard sync == nil, !busy, !closing, let profile else { return }
        busy = true
        let task = Task { await performStart(profile) }
        startup = task
        await task.value
    }

    private func performStart(_ profile: DemoProfile) async {
        defer { busy = false }
        do {
            let directory = try stateDirectory(profile)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
            let journal = directory.appendingPathComponent("notes-inbox.json")
            if FileManager.default.fileExists(atPath: journal.path) {
                notes = try DemoInbox.notes(in: JSONDecoder().decode(LibreSyncInbox.self, from: Data(contentsOf: journal)))
            }
            try await open(name: "")
        } catch { self.error = error.localizedDescription }
    }

    private func open(name: String) async throws {
        guard let profile else { return }
        let manifest = LibreSyncManifest(appID: "io.libresync.NearbySyncDemo", displayName: "Nearby Notes",
            adapters: [.init(id: "notes", namespace: "notes", schema: "logical-records-v1")])
        let config = try LibreSyncSessionConfiguration(policy: .records, manifest: manifest,
            stateDirectory: stateDirectory(profile).appendingPathComponent("session"), displayName: name)
        let session = try await openSession(config, LibreSyncKeychainStore(service: profile.keychainService))
        guard !closing else { await session.close(); return }
        let model = LibreSyncSessionModel(session: session)
        sync = model
        await model.startAndWait()
        guard !closing else { return }
        if let failure = model.error { error = failure }
        poller = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if !self.busy && !self.closing {
                    self.busy = true
                    do { try await self.applyInbox(session) }
                    catch { if !Task.isCancelled { self.error = error.localizedDescription } }
                    self.busy = false
                }
                do { try await Task.sleep(nanoseconds: 500_000_000) } catch { return }
            }
        }
    }

    private func applyInbox(_ session: LibreSyncSession) async throws {
        guard let profile else { return }
        let inbox = try await session.inbox()
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let encoded = try encoder.encode(inbox)
        guard encoded != lastInbox else { return }
        notes = try await DemoInbox.apply(inbox, to: stateDirectory(profile).appendingPathComponent("notes-inbox.json")) {
            try await session.acknowledge(inbox)
        }
        lastInbox = encoded
    }

    func save(id: String?, title: String, body: String) {
        guard !busy, !closing, let session = sync?.session else { return }
        busy = true
        operation = Task {
            defer { busy = false }
            do {
                let note = DemoNote(id: id ?? UUID().uuidString, title: title.trimmingCharacters(in: .whitespacesAndNewlines), body: body, updatedAt: Date())
                try await session.set(adapter: "notes", id: note.id, value: JSONEncoder().encode(note))
                try await applyInbox(session)
                error = nil
            } catch { self.error = error.localizedDescription }
        }
    }

    func rename(_ value: String) {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !busy, !closing else { return }
        guard !value.isEmpty, value.utf8.count <= 128,
              value.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) else {
            error = "Use a device name of 1–128 UTF-8 bytes without control characters."
            return
        }
        busy = true
        operation = Task {
            defer { busy = false }
            poller?.cancel(); await poller?.value; poller = nil
            if let sync { await sync.stopAndWait(); await sync.session.close() }
            sync = nil; lastInbox = nil
            do { try await open(name: value); error = nil }
            catch { self.error = error.localizedDescription }
        }
    }

    func stop() async {
        closing = true
        // Startup never calls stop; awaiting it here cannot wait on this task.
        await startup?.value
        await operation?.value
        poller?.cancel(); await poller?.value; poller = nil
        if let sync { await sync.stopAndWait(); await sync.session.close() }
        sync = nil
    }
}

@MainActor
final class DemoAppDelegate: NSObject, NSApplicationDelegate {
    weak var model: DemoModel?
    private var terminating = false
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminating else { return .terminateLater }
        terminating = true
        Task { await model?.stop(); sender.reply(toApplicationShouldTerminate: true) }
        return .terminateLater
    }
}
