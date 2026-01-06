package libresync

interface LibreSyncKeyStorage {
    fun store(keyId: String, data: ByteArray)
    fun load(keyId: String): ByteArray?
    fun delete(keyId: String)
}
