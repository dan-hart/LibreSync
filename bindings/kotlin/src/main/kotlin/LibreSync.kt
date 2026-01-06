package libresync

object LibreSyncNative {
    init {
        System.loadLibrary("libresync_ffi")
    }

    external fun libresync_abi_version(): Int
    external fun libresync_engine_create(configJson: String, statePath: String): Long
    external fun libresync_engine_free(handle: Long)

    external fun libresync_engine_register_json_adapter(handle: Long, adapterId: String, path: String): Boolean
    external fun libresync_engine_register_logical_file_adapter(handle: Long, adapterId: String, namespaceName: String, path: String): Boolean
    external fun libresync_engine_register_sqlite_adapter(handle: Long, adapterId: String, path: String, pageDelta: Long): Boolean

    external fun libresync_engine_start_listening(handle: Long): Boolean
    external fun libresync_engine_stop_listening(handle: Long): Boolean

    external fun libresync_engine_discover(handle: Long, timeoutMs: Long): String?
    external fun libresync_engine_pair(handle: Long, address: String): String?
    external fun libresync_engine_sync_now(handle: Long, address: String, adapterId: String): Boolean

    external fun libresync_backup_snapshot(handle: Long, adapterId: String, note: String?): String?
    external fun libresync_backup_list(handle: Long, adapterId: String): String?
    external fun libresync_backup_preview(handle: Long, adapterId: String, snapshotId: String): String?
    external fun libresync_backup_restore(handle: Long, adapterId: String, snapshotId: String): Boolean
    external fun libresync_backup_prune(handle: Long, adapterId: String, maxSnapshots: Long, maxAgeDays: Long): Boolean

    external fun libresync_last_error(): String?
}

class LibreSyncEngine(configJson: String, statePath: String) {
    private val handle: Long = LibreSyncNative.libresync_engine_create(configJson, statePath)

    init {
        require(handle != 0L) { LibreSyncNative.libresync_last_error() ?: "engine create failed" }
    }

    fun close() {
        LibreSyncNative.libresync_engine_free(handle)
    }

    fun registerJsonAdapter(id: String, path: String) {
        require(LibreSyncNative.libresync_engine_register_json_adapter(handle, id, path)) {
            LibreSyncNative.libresync_last_error() ?: "register json adapter failed"
        }
    }

    fun registerLogicalFileAdapter(id: String, namespace: String, path: String) {
        require(LibreSyncNative.libresync_engine_register_logical_file_adapter(handle, id, namespace, path)) {
            LibreSyncNative.libresync_last_error() ?: "register logical adapter failed"
        }
    }

    fun registerSqliteAdapter(id: String, path: String, pageDelta: Long = 0) {
        require(LibreSyncNative.libresync_engine_register_sqlite_adapter(handle, id, path, pageDelta)) {
            LibreSyncNative.libresync_last_error() ?: "register sqlite adapter failed"
        }
    }

    fun startListening() {
        require(LibreSyncNative.libresync_engine_start_listening(handle)) {
            LibreSyncNative.libresync_last_error() ?: "start listening failed"
        }
    }

    fun stopListening() {
        require(LibreSyncNative.libresync_engine_stop_listening(handle)) {
            LibreSyncNative.libresync_last_error() ?: "stop listening failed"
        }
    }

    fun discover(timeoutMs: Long = 3000): String {
        return LibreSyncNative.libresync_engine_discover(handle, timeoutMs)
            ?: throw IllegalStateException(LibreSyncNative.libresync_last_error() ?: "discover failed")
    }

    fun pair(address: String): String {
        return LibreSyncNative.libresync_engine_pair(handle, address)
            ?: throw IllegalStateException(LibreSyncNative.libresync_last_error() ?: "pair failed")
    }

    fun syncNow(address: String, adapterId: String) {
        require(LibreSyncNative.libresync_engine_sync_now(handle, address, adapterId)) {
            LibreSyncNative.libresync_last_error() ?: "sync failed"
        }
    }
}
