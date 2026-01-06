import libresync.LibreSyncEngine

fun main() {
    val config = """
        {
          "device_id": "kotlin-device",
          "app_id": "com.example.notes",
          "user_id": "kotlin-user",
          "listen_addr": "0.0.0.0:52345",
          "app_key": "<base64-app-key>",
          "device_cert_der": "<base64-cert>",
          "device_key_der": "<base64-key>",
          "allowlist": []
        }
    """.trimIndent()

    val engine = LibreSyncEngine(config, "./libresync.state")
    engine.registerLogicalFileAdapter("records", "com.example.notes", "./records.json")
    engine.startListening()
    println("Listening for device connections.")
}
