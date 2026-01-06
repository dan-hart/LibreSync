import libresync.LibreSyncConfig
import libresync.LibreSyncEngine
import libresync.LibreSyncKeyManager
import libresync.LibreSyncKeyStorage

fun main() {
    val storage = object : LibreSyncKeyStorage {
        private val data = mutableMapOf<String, ByteArray>()

        override fun store(keyId: String, data: ByteArray) {
            this.data[keyId] = data
        }

        override fun load(keyId: String): ByteArray? {
            return data[keyId]
        }

        override fun delete(keyId: String) {
            data.remove(keyId)
        }
    }

    val deviceId = "kotlin-device"
    val appId = "com.example.notes"
    val userId = "kotlin-user"
    val keys = LibreSyncKeyManager.loadOrCreate(deviceId, appId, userId, storage)
    val config = LibreSyncConfig(
        deviceId = deviceId,
        appId = appId,
        userId = userId,
        listenAddr = "0.0.0.0:52345",
        appKey = keys.appKey,
        deviceCertDer = keys.deviceCertDer,
        deviceKeyDer = keys.deviceKeyDer,
        allowlist = emptyList()
    ).toJson()

    val engine = LibreSyncEngine(config, "./libresync.state")
    engine.registerLogicalFileAdapter("records", "com.example.notes", "./records.json")
    engine.startListening()
    println("Listening for device connections.")
}
