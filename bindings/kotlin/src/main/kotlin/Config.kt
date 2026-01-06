package libresync

data class LibreSyncAllowlistEntry(
    val deviceId: String,
    val fingerprint: String
)

data class LibreSyncConfig(
    val deviceId: String,
    val appId: String,
    val userId: String,
    val listenAddr: String? = null,
    val appKey: String,
    val deviceCertDer: String,
    val deviceKeyDer: String,
    val allowlist: List<LibreSyncAllowlistEntry> = emptyList(),
    val autoAccept: Boolean = false,
    val pairingSecret: String? = null
) {
    fun toJson(): String {
        val builder = StringBuilder()
        builder.append("{")
        builder.append("\"device_id\":\"").append(jsonEscape(deviceId)).append("\",")
        builder.append("\"app_id\":\"").append(jsonEscape(appId)).append("\",")
        builder.append("\"user_id\":\"").append(jsonEscape(userId)).append("\",")
        listenAddr?.let {
            builder.append("\"listen_addr\":\"").append(jsonEscape(it)).append("\",")
        }
        builder.append("\"app_key\":\"").append(jsonEscape(appKey)).append("\",")
        builder.append("\"device_cert_der\":\"").append(jsonEscape(deviceCertDer)).append("\",")
        builder.append("\"device_key_der\":\"").append(jsonEscape(deviceKeyDer)).append("\",")
        builder.append("\"allowlist\":[")
        for ((index, entry) in allowlist.withIndex()) {
            if (index > 0) builder.append(",")
            builder.append("{")
            builder.append("\"device_id\":\"").append(jsonEscape(entry.deviceId)).append("\",")
            builder.append("\"fingerprint\":\"").append(jsonEscape(entry.fingerprint)).append("\"")
            builder.append("}")
        }
        builder.append("],")
        builder.append("\"auto_accept\":").append(autoAccept)
        pairingSecret?.let {
            builder.append(",\"pairing_secret\":\"").append(jsonEscape(it)).append("\"")
        }
        builder.append("}")
        return builder.toString()
    }
}

private fun jsonEscape(value: String): String {
    return value
        .replace("\\", "\\\\")
        .replace("\"", "\\\"")
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
}
