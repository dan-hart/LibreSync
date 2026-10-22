#if os(macOS)
import AppKit
#endif
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
    private var lifecycle: Task<Void, Never>?
    private var active = true
    private var busyCount = 0

    private func beginWork() { busyCount += 1; busy = true }
    private func endWork() { busyCount -= 1; busy = busyCount > 0 }
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

    // All lifecycle work shares one queue. Desired activity changes immediately,
    // so an in-flight open/rename cannot start discovery after backgrounding.
    private func enqueue(_ work: @escaping @MainActor () async -> Void) -> Task<Void, Never> {
        let previous = lifecycle
        let task = Task { await previous?.value; await work() }
        lifecycle = task
        return task
    }

    func start() async {
        guard !closing else { return }
        let task = enqueue { [self] in
            guard sync == nil, !closing, let profile else { return }
            beginWork()
            await performStart(profile)
        }
        await task.value
    }

    func setActive(_ value: Bool) async {
        guard !closing else { return }
        active = value
        let task = enqueue { [self] in
            guard !closing else { return }
            await reconcileActivity()
        }
        await task.value
    }

    private func reconcileActivity() async {
        poller?.cancel(); await poller?.value; poller = nil
        guard let sync else { return }
        if active && !closing {
            await sync.startAndWait()
            guard active && !closing else { await sync.stopAndWait(); return }
            if let failure = sync.error { error = failure }
            beginPolling(sync.session)
        } else {
            await sync.stopAndWait()
        }
    }

    private func performStart(_ profile: DemoProfile) async {
        defer { endWork() }
        do {
            let directory = try stateDirectory(profile)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
            #if os(iOS)
            var protectedDirectory = directory
            var resourceValues = URLResourceValues()
            resourceValues.isExcludedFromBackup = true
            try protectedDirectory.setResourceValues(resourceValues)
            // Newly created journals inherit protection before they contain data.
            try FileManager.default.setAttributes([.protectionKey: FileProtectionType.complete], ofItemAtPath: directory.path)
            #endif
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
        #if os(iOS)
        let deviceKind = "mobile"
        #else
        let deviceKind = "desktop"
        #endif
        let config = try LibreSyncSessionConfiguration(policy: .records, manifest: manifest,
            stateDirectory: stateDirectory(profile).appendingPathComponent("session"), displayName: name, deviceKind: deviceKind)
        let session = try await openSession(config, LibreSyncKeychainStore(service: profile.keychainService))
        guard !closing else { await session.close(); return }
        let model = LibreSyncSessionModel(session: session)
        sync = model
        await reconcileActivity()
    }

    private func beginPolling(_ session: LibreSyncSession) {
        poller = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if self.active && !self.busy && !self.closing {
                    self.beginWork()
                    do { try await self.applyInbox(session) }
                    catch { if !Task.isCancelled { self.error = error.localizedDescription } }
                    self.endWork()
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
        guard active, !busy, !closing, let session = sync?.session else { return }
        beginWork()
        _ = enqueue { [self] in
            defer { endWork() }
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
        guard active, !busy, !closing else { return }
        guard !value.isEmpty, value.utf8.count <= 128,
              value.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) else {
            error = "Use a device name of 1–128 UTF-8 bytes without control characters."
            return
        }
        beginWork()
        _ = enqueue { [self] in
            defer { endWork() }
            poller?.cancel(); await poller?.value; poller = nil
            if let sync { await sync.stopAndWait(); await sync.session.close() }
            sync = nil; lastInbox = nil
            do { try await open(name: value); error = nil }
            catch { self.error = error.localizedDescription }
        }
    }

    func stop() async {
        closing = true
        active = false
        let task = enqueue { [self] in
            poller?.cancel(); await poller?.value; poller = nil
            if let sync { await sync.stopAndWait(); await sync.session.close() }
            sync = nil
        }
        await task.value
    }
}

#if os(macOS)
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

#endif
