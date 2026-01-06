package libresync

enum class LibreSyncSqliteLogicalEncoding(private val jsonValue: String) {
    PLAIN("plain"),
    BOOL("bool"),
    JSON("json"),
    JSON_VALUE("json_value");

    fun toJson(): String = jsonValue
}

sealed class LibreSyncMergePolicy {
    object LastWriterWins : LibreSyncMergePolicy()
    object SetUnion : LibreSyncMergePolicy()
    object Counter : LibreSyncMergePolicy()
    object ListAppend : LibreSyncMergePolicy()
    data class Custom(val name: String) : LibreSyncMergePolicy()

    fun toJsonValue(): String {
        return when (this) {
            LastWriterWins -> "\"last_writer_wins\""
            SetUnion -> "\"set_union\""
            Counter -> "\"counter\""
            ListAppend -> "\"list_append\""
            is Custom -> "{\"custom\":\"${jsonEscape(name)}\"}"
        }
    }
}

data class LibreSyncSqliteLogicalField(
    val column: String,
    val field: String,
    val encoding: LibreSyncSqliteLogicalEncoding? = null,
    val mergePolicy: LibreSyncMergePolicy? = null
)

data class LibreSyncSqliteLogicalMapping(
    val dataTable: String,
    val idColumn: String,
    val schema: String,
    val entity: String,
    val fields: List<LibreSyncSqliteLogicalField>,
    val metaTable: String? = null,
    val defaultMergePolicy: LibreSyncMergePolicy? = null
) {
    fun toJson(): String {
        val builder = StringBuilder()
        builder.append("{")
        builder.append("\"data_table\":\"").append(jsonEscape(dataTable)).append("\",")
        builder.append("\"id_column\":\"").append(jsonEscape(idColumn)).append("\",")
        builder.append("\"schema\":\"").append(jsonEscape(schema)).append("\",")
        builder.append("\"entity\":\"").append(jsonEscape(entity)).append("\",")
        builder.append("\"fields\":[")
        for ((index, field) in fields.withIndex()) {
            if (index > 0) builder.append(",")
            builder.append("{")
            builder.append("\"column\":\"").append(jsonEscape(field.column)).append("\",")
            builder.append("\"field\":\"").append(jsonEscape(field.field)).append("\"")
            field.encoding?.let { builder.append(",\"encoding\":\"").append(it.toJson()).append("\"") }
            field.mergePolicy?.let { builder.append(",\"merge_policy\":").append(it.toJsonValue()) }
            builder.append("}")
        }
        builder.append("]")
        metaTable?.let { builder.append(",\"meta_table\":\"").append(jsonEscape(it)).append("\"") }
        defaultMergePolicy?.let { builder.append(",\"default_merge_policy\":").append(it.toJsonValue()) }
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
