import Foundation
import CLibreSync

public struct LibreSyncDeviceInfo: Codable {
    public let device_id: String
    public let user_id: String
    public let app_id: String
    public let address: String?
    public let linked: Bool
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

    public func syncNow(address: String, adapterId: String) throws {
        guard libresync_engine_sync_now(handle, address, adapterId) else {
            throw LibreSyncError.message(Self.lastError())
        }
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
