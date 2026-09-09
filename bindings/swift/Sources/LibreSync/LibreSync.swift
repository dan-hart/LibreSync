import Foundation
import CLibreSync

public struct LibreSyncDeviceInfo: Codable {
    public let device_id: String
    public let user_id: String
    public let app_id: String
    public let address: String?
    public let linked: Bool
    /// SHA-256 fingerprint of the device certificate, when known. Pin it after
    /// linking and treat a `fingerprint_changed` event as a user decision.
    public let fingerprint: String?
}

/// One engine event. `type` is a snake_case tag (`sync_started`,
/// `sync_finished`, `sync_stats`, `fingerprint_changed`, `inbound_sync`,
/// `link_finished`, `discovery_finished`, `listener_started`,
/// `listener_stopped`, `task_finished`, `device_seen`, `error`, ...); the
/// remaining keys are the event's fields.
public struct LibreSyncEvent {
    public let type: String
    public let fields: [String: Any]

    public var device: LibreSyncDeviceInfo? {
        guard let raw = fields["device"], JSONSerialization.isValidJSONObject(raw),
              let data = try? JSONSerialization.data(withJSONObject: raw) else { return nil }
        return try? JSONDecoder().decode(LibreSyncDeviceInfo.self, from: data)
    }

    public var ticket: UInt64? {
        (fields["ticket"] as? NSNumber)?.uint64Value
    }

    public var succeeded: Bool? {
        (fields["result"] as? [String: Any])?["ok"] as? Bool
    }

    public var errorMessage: String? {
        if let message = fields["message"] as? String { return message }
        if let error = fields["error"] as? String { return error }
        return (fields["result"] as? [String: Any])?["error"] as? String
    }

    static func decode(json: String) -> LibreSyncEvent? {
        guard let data = json.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = object["type"] as? String else { return nil }
        return LibreSyncEvent(type: type, fields: object)
    }
}

public enum LibreSyncError: Error, CustomStringConvertible {
    case message(String)

    public var description: String {
        switch self {
        case .message(let value): return value
        }
    }
}

public final class LibreSyncEngine {
    private var handle: UnsafeMutableRawPointer?

    public init(configJson: String, statePath: String) throws {
        let handle = libresync_engine_create(configJson, statePath)
        if handle == nil {
            throw LibreSyncError.message(LibreSyncEngine.lastError())
        }
        self.handle = handle
    }

    deinit {
        if let handle {
            libresync_engine_free(handle)
        }
    }

    public func startListening() throws {
        guard libresync_engine_start_listening(handle) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func stopListening() throws {
        guard libresync_engine_stop_listening(handle) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func registerJsonAdapter(id: String, path: String) throws {
        guard libresync_engine_register_json_adapter(handle, id, path) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func registerLogicalFileAdapter(id: String, namespace: String, path: String) throws {
        guard libresync_engine_register_logical_file_adapter(handle, id, namespace, path) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func registerSqliteAdapter(id: String, path: String, pageDelta: Int) throws {
        guard libresync_engine_register_sqlite_adapter(handle, id, path, pageDelta) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func registerSqliteLogicalAdapter(id: String, namespace: String, path: String, mappingJson: String) throws {
        guard libresync_engine_register_sqlite_logical_adapter(handle, id, namespace, path, mappingJson) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func registerSqliteLogicalAdapter(
        id: String,
        namespace: String,
        path: String,
        mappings: [LibreSyncSqliteLogicalMapping]
    ) throws {
        if mappings.isEmpty {
            throw LibreSyncError.message("mappings must not be empty")
        }
        let data = try JSONEncoder().encode(mappings)
        let mappingJson = String(decoding: data, as: UTF8.self)
        try registerSqliteLogicalAdapter(id: id, namespace: namespace, path: path, mappingJson: mappingJson)
    }

    public func discover(timeoutMs: UInt64 = 3000) throws -> [LibreSyncDeviceInfo] {
        guard let jsonPtr = libresync_engine_discover(handle, timeoutMs) else {
            throw LibreSyncError.message(Self.lastError())
        }
        defer { libresync_string_free(jsonPtr) }
        let json = String(cString: jsonPtr)
        return try JSONDecoder().decode([LibreSyncDeviceInfo].self, from: Data(json.utf8))
    }

    public func link(address: String) throws -> LibreSyncDeviceInfo {
        guard let jsonPtr = libresync_engine_link(handle, address) else {
            throw LibreSyncError.message(Self.lastError())
        }
        defer { libresync_string_free(jsonPtr) }
        let json = String(cString: jsonPtr)
        return try JSONDecoder().decode(LibreSyncDeviceInfo.self, from: Data(json.utf8))
    }

    /// Blocking sync. Call from a background queue, never the main thread.
    public func syncNow(address: String, adapterId: String) throws {
        guard libresync_engine_sync_now(handle, address, adapterId) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    /// Non-blocking sync. Returns a ticket; completion is reported by a
    /// `task_finished` event carrying the same ticket (after `sync_finished`).
    @discardableResult
    public func syncAsync(address: String, adapterId: String) throws -> UInt64 {
        let ticket = libresync_engine_sync_async(handle, address, adapterId)
        if ticket == 0 {
            throw LibreSyncError.message(Self.lastError())
        }
        return ticket
    }

    /// Cancels a running asynchronous sync.
    @discardableResult
    public func cancel(ticket: UInt64) -> Bool {
        libresync_engine_cancel(handle, ticket)
    }

    /// Next queued event, or nil when the queue is empty. Never blocks.
    public func nextEvent() -> LibreSyncEvent? {
        guard let ptr = libresync_engine_event_next(handle) else { return nil }
        defer { libresync_string_free(ptr) }
        return LibreSyncEvent.decode(json: String(cString: ptr))
    }

    /// Blocks up to `timeoutMs` for the next event. Background queues only.
    public func waitEvent(timeoutMs: UInt64) -> LibreSyncEvent? {
        guard let ptr = libresync_engine_event_wait(handle, timeoutMs) else { return nil }
        defer { libresync_string_free(ptr) }
        return LibreSyncEvent.decode(json: String(cString: ptr))
    }

    /// Descriptor that is readable while events are queued (-1 if unavailable).
    public var eventFileDescriptor: Int32 {
        libresync_engine_event_fd(handle)
    }

    /// Delivers events on `queue` (main by default) without polling, using a
    /// `DispatchSource` on the event descriptor. Keep the returned pump alive
    /// for as long as you want events; it stops when deallocated.
    public func makeEventPump(
        queue: DispatchQueue = .main,
        handler: @escaping (LibreSyncEvent) -> Void
    ) -> LibreSyncEventPump {
        LibreSyncEventPump(engine: self, queue: queue, handler: handler)
    }

    public func saveState() throws {
        guard libresync_engine_save_state(handle) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func backupSnapshot(adapterId: String, note: String? = nil) throws -> String {
        let ptr = libresync_backup_snapshot(handle, adapterId, note)
        guard let jsonPtr = ptr else {
            throw LibreSyncError.message(Self.lastError())
        }
        defer { libresync_string_free(jsonPtr) }
        return String(cString: jsonPtr)
    }

    public func backupList(adapterId: String) throws -> String {
        guard let jsonPtr = libresync_backup_list(handle, adapterId) else {
            throw LibreSyncError.message(Self.lastError())
        }
        defer { libresync_string_free(jsonPtr) }
        return String(cString: jsonPtr)
    }

    public func backupPreview(adapterId: String, snapshotId: String) throws -> String {
        guard let jsonPtr = libresync_backup_preview(handle, adapterId, snapshotId) else {
            throw LibreSyncError.message(Self.lastError())
        }
        defer { libresync_string_free(jsonPtr) }
        return String(cString: jsonPtr)
    }

    public func backupRestore(adapterId: String, snapshotId: String) throws {
        guard libresync_backup_restore(handle, adapterId, snapshotId) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public func backupPrune(adapterId: String, maxSnapshots: Int, maxAgeDays: UInt64) throws {
        guard libresync_backup_prune(handle, adapterId, maxSnapshots, maxAgeDays) else {
            throw LibreSyncError.message(Self.lastError())
        }
    }

    public static func lastError() -> String {
        guard let ptr = libresync_last_error() else { return "unknown error" }
        defer { libresync_string_free(ptr) }
        return String(cString: ptr)
    }
}

/// Wakes up when the engine's event descriptor becomes readable and drains
/// the queue onto a dispatch queue. This is the SwiftUI / AppKit integration
/// pattern: the engine runs on its own threads, the pump hands events to the
/// main queue, and nothing polls.
public final class LibreSyncEventPump {
    private let engine: LibreSyncEngine
    private let queue: DispatchQueue
    private let handler: (LibreSyncEvent) -> Void
    private var source: DispatchSourceRead?
    private var fallback: DispatchSourceTimer?

    init(engine: LibreSyncEngine, queue: DispatchQueue, handler: @escaping (LibreSyncEvent) -> Void) {
        self.engine = engine
        self.queue = queue
        self.handler = handler
        let fd = engine.eventFileDescriptor
        if fd >= 0 {
            let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
            source.setEventHandler { [weak self] in self?.drain() }
            source.resume()
            self.source = source
        } else {
            // No descriptor on this platform: fall back to a coarse timer.
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now(), repeating: .milliseconds(250))
            timer.setEventHandler { [weak self] in self?.drain() }
            timer.resume()
            self.fallback = timer
        }
    }

    deinit {
        source?.cancel()
        fallback?.cancel()
    }

    private func drain() {
        while let event = engine.nextEvent() {
            handler(event)
        }
    }
}
