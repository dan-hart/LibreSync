//! JNI symbols, JavaVM attachment and global secure-store ownership.
use jni::{
    objects::{GlobalRef, JByteArray, JObject, JString, JValue},
    sys::{jlong, jstring},
    JNIEnv, JavaVM,
};
use libresync::{Error, KeyStore, Result, SessionErrorCode};
use std::sync::Arc;
struct JavaKeys {
    vm: JavaVM,
    object: GlobalRef,
}
fn unavailable(message: impl ToString) -> Error {
    Error::Managed {
        code: SessionErrorCode::StorageUnavailable,
        message: message.to_string(),
    }
}
impl KeyStore for JavaKeys {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut env = self.vm.attach_current_thread().map_err(unavailable)?;
        let name = env.new_string(name).map_err(unavailable)?;
        let value = env.call_method(
            self.object.as_obj(),
            "load",
            "(Ljava/lang/String;)[B",
            &[JValue::Object(&name)],
        );
        let value = match value {
            Ok(v) => v.l().map_err(unavailable)?,
            Err(e) => {
                let _ = env.exception_clear();
                return Err(unavailable(e));
            }
        };
        if value.is_null() {
            Ok(None)
        } else {
            let bytes = JByteArray::from(value);
            if env.get_array_length(&bytes).map_err(unavailable)? > 1024 * 1024 {
                return Err(unavailable("Secure storage item exceeds 1 MiB"));
            }
            env.convert_byte_array(bytes).map(Some).map_err(unavailable)
        }
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        let mut env = self.vm.attach_current_thread().map_err(unavailable)?;
        let name = env.new_string(name).map_err(unavailable)?;
        let value = env.byte_array_from_slice(value).map_err(unavailable)?;
        match env.call_method(
            self.object.as_obj(),
            "store",
            "(Ljava/lang/String;[B)V",
            &[JValue::Object(&name), JValue::Object(&value)],
        ) {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = env.exception_clear();
                Err(unavailable(e))
            }
        }
    }
    fn delete(&self, name: &str) -> Result<()> {
        let mut env = self.vm.attach_current_thread().map_err(unavailable)?;
        let name = env.new_string(name).map_err(unavailable)?;
        match env.call_method(
            self.object.as_obj(),
            "delete",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&name)],
        ) {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = env.exception_clear();
                Err(unavailable(e))
            }
        }
    }
}
fn output(env: &mut JNIEnv<'_>, value: serde_json::Value) -> jstring {
    match env.new_string(value.to_string()) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
fn failure(message: impl ToString) -> serde_json::Value {
    serde_json::json!({"abi":1,"ok":false,"error":{"code":"InvalidOperation","message":message.to_string()}})
}
#[no_mangle]
pub extern "system" fn Java_libresync_ManagedNative_open(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    config: JString<'_>,
    keys: JObject<'_>,
) -> jstring {
    let result = (|| -> std::result::Result<serde_json::Value, String> {
        let text: String = env.get_string(&config).map_err(|e| e.to_string())?.into();
        if text.len() > 16 * 1024 {
            return Err("configuration too large".into());
        }
        let config = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let keys = JavaKeys {
            vm: env.get_java_vm().map_err(|e| e.to_string())?,
            object: env.new_global_ref(keys).map_err(|e| e.to_string())?,
        };
        Ok(super::managed::open(config, Arc::new(keys)))
    })();
    output(&mut env, result.unwrap_or_else(failure))
}
#[no_mangle]
pub extern "system" fn Java_libresync_ManagedNative_call(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    handle: jlong,
    command: JString<'_>,
) -> jstring {
    let result = (|| -> std::result::Result<serde_json::Value, String> {
        let text: String = env.get_string(&command).map_err(|e| e.to_string())?.into();
        if text.len() > 128 * 1024 * 1024 {
            return Ok(
                serde_json::json!({"abi":1,"ok":false,"error":{"code":"InvalidRecord","message":"Serialized record request exceeds 128 MiB"}}),
            );
        }
        let command = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(super::managed::dispatch(handle as u64, command))
    })();
    output(&mut env, result.unwrap_or_else(failure))
}
#[no_mangle]
pub extern "system" fn Java_libresync_ManagedNative_close(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    handle: jlong,
) -> jstring {
    output(&mut env, super::managed::close(handle as u64))
}
fn legacy_string(env: &mut JNIEnv<'_>, ptr: *mut std::ffi::c_char) -> jstring {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    super::libresync_string_free(ptr);
    match env.new_string(text) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
#[export_name = "Java_libresync_LibreSyncNative_libresync_1abi_1version"]
pub extern "system" fn legacy_abi(_: JNIEnv<'_>, _: JObject<'_>) -> jni::sys::jint {
    super::libresync_abi_version() as i32
}
#[export_name = "Java_libresync_LibreSyncNative_libresync_1generate_1app_1key"]
pub extern "system" fn legacy_app_key(mut env: JNIEnv<'_>, _: JObject<'_>) -> jstring {
    legacy_string(&mut env, super::libresync_generate_app_key())
}
#[export_name = "Java_libresync_LibreSyncNative_libresync_1last_1error"]
pub extern "system" fn legacy_error(mut env: JNIEnv<'_>, _: JObject<'_>) -> jstring {
    legacy_string(&mut env, super::libresync_last_error())
}
#[export_name = "Java_libresync_LibreSyncNative_libresync_1generate_1device_1keys"]
pub extern "system" fn legacy_device_keys(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    device: JString<'_>,
    app: JString<'_>,
    user: JString<'_>,
) -> jstring {
    let strings = (|| -> std::result::Result<_, String> {
        let d: String = env.get_string(&device).map_err(|e| e.to_string())?.into();
        let a: String = env.get_string(&app).map_err(|e| e.to_string())?.into();
        let u: String = env.get_string(&user).map_err(|e| e.to_string())?.into();
        Ok((
            std::ffi::CString::new(d).map_err(|e| e.to_string())?,
            std::ffi::CString::new(a).map_err(|e| e.to_string())?,
            std::ffi::CString::new(u).map_err(|e| e.to_string())?,
        ))
    })();
    match strings {
        Ok((d, a, u)) => legacy_string(
            &mut env,
            super::libresync_generate_device_keys(d.as_ptr(), a.as_ptr(), u.as_ptr()),
        ),
        Err(_) => std::ptr::null_mut(),
    }
}
