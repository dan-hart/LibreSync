import libresync.LibreSyncConfig
import libresync.LibreSyncEngine
import libresync.LibreSyncKeyManager
import libresync.LibreSyncKeyStorage
import libresync.LibreSyncSqliteLogicalEncoding
import libresync.LibreSyncSqliteLogicalField
import libresync.LibreSyncSqliteLogicalMapping

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

    val deviceId = "room-device"
    val appId = "com.example.room.notes"
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

    val noteMapping = LibreSyncSqliteLogicalMapping(
        dataTable = "notes",
        idColumn = "id",
        schema = "notes",
        entity = "Note",
        fields = listOf(
            LibreSyncSqliteLogicalField("title", "title"),
            LibreSyncSqliteLogicalField("body", "body"),
            LibreSyncSqliteLogicalField("is_archived", "is_archived", LibreSyncSqliteLogicalEncoding.BOOL),
            LibreSyncSqliteLogicalField("updated_at", "updated_at")
        )
    )

    val tagMapping = LibreSyncSqliteLogicalMapping(
        dataTable = "tags",
        idColumn = "id",
        schema = "notes",
        entity = "Tag",
        fields = listOf(
            LibreSyncSqliteLogicalField("name", "name"),
            LibreSyncSqliteLogicalField("palette", "palette")
        )
    )

    val engine = LibreSyncEngine(config, "./room.state")
    engine.registerSqliteLogicalAdapter("room", appId, "./room.db", listOf(noteMapping, tagMapping))
    engine.startListening()
    println("Room sample listening with multi-table SQLite logical mappings.")
}
