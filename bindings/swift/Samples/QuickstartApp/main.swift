import Foundation
import LibreSync

// Trigger the Local Network permission prompt on Apple platforms.
LibreSyncLocalNetworkPermission().request { allowed in
    print("Local Network permission allowed: \(allowed)")
}

let config = """
{
  "device_id": "swift-device",
  "app_id": "com.example.notes",
  "user_id": "swift-user",
  "listen_addr": "0.0.0.0:52345",
  "app_key": "<base64-app-key>",
  "device_cert_der": "<base64-cert>",
  "device_key_der": "<base64-key>",
  "allowlist": []
}
"""

let statePath = FileManager.default.temporaryDirectory.appendingPathComponent("libresync.state").path

let engine = try LibreSyncEngine(configJson: config, statePath: statePath)
try engine.registerLogicalFileAdapter(id: "records", namespace: "com.example.notes", path: "./records.json")
try engine.startListening()

print("Listening. Run pairing from another device.")
