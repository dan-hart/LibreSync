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

// Events are delivered on the main queue without polling.
let pump = engine.makeEventPump { event in
    switch event.type {
    case "fingerprint_changed":
        // Never re-pin silently: ask the user, then re-link if they agree.
        print("certificate changed for \(event.device?.device_id ?? "?"): \(event.fields["fingerprint"] ?? "")")
    case "sync_finished":
        print("sync with \(event.device?.device_id ?? "?") ok=\(event.succeeded ?? false)")
    case "task_finished":
        print("task \(event.ticket ?? 0) finished ok=\(event.succeeded ?? false)")
    default:
        print("event: \(event.type)")
    }
}

try engine.startListening()
print("Listening. Run linking from another device.")

// Non-blocking sync from the UI: completion arrives as events.
if let peer = CommandLine.arguments.dropFirst().first {
    let ticket = try engine.syncAsync(address: peer, adapterId: "records")
    print("queued sync ticket \(ticket)")
}

withExtendedLifetime(pump) {
    RunLoop.main.run()
}
