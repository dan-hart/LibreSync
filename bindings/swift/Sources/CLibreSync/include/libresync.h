#ifndef LIBRESYNC_H
#define LIBRESYNC_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint32_t libresync_abi_version(void);

void *libresync_engine_create(const char *config_json, const char *state_path);
void libresync_engine_free(void *handle);

bool libresync_engine_set_auto_accept(void *handle, bool enabled);

bool libresync_engine_register_json_adapter(void *handle, const char *adapter_id, const char *path);
bool libresync_engine_register_logical_file_adapter(void *handle, const char *adapter_id, const char *namespace_name, const char *path);
bool libresync_engine_register_sqlite_adapter(void *handle, const char *adapter_id, const char *path, size_t page_delta);

bool libresync_engine_start_listening(void *handle);
bool libresync_engine_stop_listening(void *handle);

char *libresync_engine_discover(void *handle, uint64_t timeout_ms);
char *libresync_engine_pair(void *handle, const char *address);

bool libresync_allowlist_add(void *handle, const char *device_id, const char *fingerprint);
bool libresync_allowlist_clear(void *handle);

bool libresync_engine_sync_now(void *handle, const char *address, const char *adapter_id);
bool libresync_engine_save_state(void *handle);

char *libresync_backup_snapshot(void *handle, const char *adapter_id, const char *note);
char *libresync_backup_list(void *handle, const char *adapter_id);
char *libresync_backup_preview(void *handle, const char *adapter_id, const char *snapshot_id);
bool libresync_backup_restore(void *handle, const char *adapter_id, const char *snapshot_id);
bool libresync_backup_prune(void *handle, const char *adapter_id, size_t max_snapshots, uint64_t max_age_days);

char *libresync_last_error(void);
void libresync_string_free(char *ptr);

#ifdef __cplusplus
} // extern "C"
#endif

#endif
