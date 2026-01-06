import Foundation
import LibreSync

// Trigger the Local Network permission prompt on Apple platforms.
LibreSyncLocalNetworkPermission().request { allowed in
    print("Local Network permission allowed: \(allowed)")
}

let deviceId = "swift-device"
let appId = "com.example.notes"
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
let configJson = try config.jsonString(pretty: true)

let statePath = FileManager.default.temporaryDirectory.appendingPathComponent("libresync.state").path

let engine = try LibreSyncEngine(configJson: configJson, statePath: statePath)
try engine.registerLogicalFileAdapter(id: "records", namespace: "com.example.notes", path: "./records.json")
try engine.startListening()

print("Listening. Run linking from another device.")
