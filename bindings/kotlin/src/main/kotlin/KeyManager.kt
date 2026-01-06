package libresync

data class LibreSyncKeyMaterial(
    val appKey: String,
    val deviceCertDer: String,
    val deviceKeyDer: String,
    val fingerprint: String
)

object LibreSyncKeyManager {
    private const val APP_KEY = "app_key"
    private const val DEVICE_CERT = "device_cert_der"
    private const val DEVICE_KEY = "device_key_der"
    private const val FINGERPRINT = "fingerprint"

    fun loadOrCreate(
        deviceId: String,
        appId: String,
        userId: String,
        storage: LibreSyncKeyStorage,
        prefix: String = "libresync"
    ): LibreSyncKeyMaterial {
        val existing = load(storage, prefix)
        if (existing != null) return existing

        val appKey = LibreSyncNative.libresync_generate_app_key()
            ?: error(LibreSyncNative.libresync_last_error() ?: "generate app key failed")
        val deviceKeysJson = LibreSyncNative.libresync_generate_device_keys(deviceId, appId, userId)
            ?: error(LibreSyncNative.libresync_last_error() ?: "generate device keys failed")
        val material = LibreSyncKeyMaterial(
            appKey = appKey,
            deviceCertDer = readJsonField(deviceKeysJson, "device_cert_der")
                ?: error("device_cert_der missing from key payload"),
            deviceKeyDer = readJsonField(deviceKeysJson, "device_key_der")
                ?: error("device_key_der missing from key payload"),
            fingerprint = readJsonField(deviceKeysJson, "fingerprint")
                ?: error("fingerprint missing from key payload")
        )
        save(storage, prefix, material)
        return material
    }

    fun clear(storage: LibreSyncKeyStorage, prefix: String = "libresync") {
        storage.delete(keyId(prefix, APP_KEY))
        storage.delete(keyId(prefix, DEVICE_CERT))
        storage.delete(keyId(prefix, DEVICE_KEY))
        storage.delete(keyId(prefix, FINGERPRINT))
    }

    private fun load(storage: LibreSyncKeyStorage, prefix: String): LibreSyncKeyMaterial? {
        val appKey = loadString(storage, keyId(prefix, APP_KEY)) ?: return null
        val cert = loadString(storage, keyId(prefix, DEVICE_CERT)) ?: return null
        val key = loadString(storage, keyId(prefix, DEVICE_KEY)) ?: return null
        val fingerprint = loadString(storage, keyId(prefix, FINGERPRINT)) ?: return null
        return LibreSyncKeyMaterial(appKey, cert, key, fingerprint)
    }

    private fun save(storage: LibreSyncKeyStorage, prefix: String, material: LibreSyncKeyMaterial) {
        storage.store(keyId(prefix, APP_KEY), material.appKey.toByteArray(Charsets.UTF_8))
        storage.store(keyId(prefix, DEVICE_CERT), material.deviceCertDer.toByteArray(Charsets.UTF_8))
        storage.store(keyId(prefix, DEVICE_KEY), material.deviceKeyDer.toByteArray(Charsets.UTF_8))
        storage.store(keyId(prefix, FINGERPRINT), material.fingerprint.toByteArray(Charsets.UTF_8))
    }

    private fun loadString(storage: LibreSyncKeyStorage, keyId: String): String? {
        return storage.load(keyId)?.toString(Charsets.UTF_8)
    }

    private fun keyId(prefix: String, name: String): String {
        return "$prefix.$name"
    }

    private fun readJsonField(json: String, key: String): String? {
        val pattern = Regex("\"$key\"\\s*:\\s*\"([^\"]*)\"")
        val match = pattern.find(json) ?: return null
        return match.groupValues.getOrNull(1)
    }
}
