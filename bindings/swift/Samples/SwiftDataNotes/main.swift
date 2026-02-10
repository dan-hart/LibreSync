import Foundation
import LibreSync

let deviceId = "swiftdata-device"
let appId = "com.example.swiftdata.notes"
let userId = "swift-user"
let keys = try LibreSyncKeyManager.loadOrCreate(deviceId: deviceId, appId: appId, userId: userId)
let config = LibreSyncConfig(
    deviceId: deviceId,
    appId: appId,
    userId: userId,
    listenAddr: "0.0.0.0:52345",
    appKey: keys.appKey,
    deviceCertDer: keys.deviceCertDer,
    deviceKeyDer: keys.deviceKeyDer
)

let workDir = FileManager.default.temporaryDirectory.appendingPathComponent("swiftdata-notes")
try FileManager.default.createDirectory(at: workDir, withIntermediateDirectories: true, attributes: nil)
let statePath = workDir.appendingPathComponent("libresync.state").path
let sqlitePath = workDir.appendingPathComponent("notes.sqlite").path

let noteMapping = LibreSyncSqliteLogicalMapping(
    dataTable: "notes",
    idColumn: "id",
    schema: "notes",
    entity: "Note",
    fields: [
        LibreSyncSqliteLogicalField(column: "title", field: "title"),
        LibreSyncSqliteLogicalField(column: "body", field: "body"),
        LibreSyncSqliteLogicalField(column: "is_archived", field: "is_archived", encoding: .bool),
        LibreSyncSqliteLogicalField(column: "updated_at", field: "updated_at")
    ]
)

let folderMapping = LibreSyncSqliteLogicalMapping(
    dataTable: "folders",
    idColumn: "id",
    schema: "notes",
    entity: "Folder",
    fields: [
        LibreSyncSqliteLogicalField(column: "name", field: "name"),
        LibreSyncSqliteLogicalField(column: "color_hex", field: "color_hex")
    ]
)

let engine = try LibreSyncEngine(configJson: try config.jsonString(), statePath: statePath)
try engine.registerSqliteLogicalAdapter(
    id: "swiftdata",
    namespace: appId,
    path: sqlitePath,
    mappings: [noteMapping, folderMapping]
)
try engine.startListening()

print("SwiftData sample listening with multi-table SQLite logical mappings.")
