package libresync
internal object ManagedNative {
 init { System.loadLibrary("libresync_ffi") }
 external fun open(config:String,keys:LibreSyncKeyStorage):String
 external fun call(handle:Long,command:String):String
 external fun close(handle:Long):String
}
