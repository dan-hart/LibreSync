package libresync
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.provider.Settings
import android.content.pm.PackageManager
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import java.util.concurrent.ConcurrentHashMap
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

object LocalNetworkPermission {
 const val permission="android.permission.ACCESS_LOCAL_NETWORK"
 fun state(context:Context):PermissionState = if(Build.VERSION.SDK_INT<37 || context.applicationInfo.targetSdkVersion<37) PermissionState.Unknown else if(context.checkSelfPermission(permission)==PackageManager.PERMISSION_GRANTED) PermissionState.Allowed else PermissionState.Denied
 fun requestRequired(context:Context)=Build.VERSION.SDK_INT>=37 && context.applicationInfo.targetSdkVersion>=37 && context.checkSelfPermission(permission)!=PackageManager.PERMISSION_GRANTED
 fun settingsIntent(context:Context)=Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS,Uri.parse("package:${context.packageName}"))
}
/** Acquire/release multicast only while the host is foreground. Session pause
 * stops its listener and scheduler when backgrounded; resume triggers reconnect. */
class SessionLifecycle(context:Context,private val session:Session):DefaultLifecycleObserver,AutoCloseable {
 private val lock=(context.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager).createMulticastLock("LibreSync").apply{setReferenceCounted(false)}
 private val scope=CoroutineScope(SupervisorJob()+Dispatchers.IO)
 private val mutableFailure=MutableStateFlow<SessionFailure?>(null)
 val failure:StateFlow<SessionFailure?> = mutableFailure.asStateFlow()
 private fun report(error:Exception){mutableFailure.value=(error as? SessionException)?.failure ?: SessionFailure(SessionErrorCode.OperationFailed,error.message?:"Platform lifecycle failed")}
 private val transitions=kotlinx.coroutines.channels.Channel<Boolean>(kotlinx.coroutines.channels.Channel.CONFLATED)
 @Volatile private var closed=false
 init { scope.launch {
  try { for(foreground in transitions) {
   try {
    if(foreground && !closed){if(!lock.isHeld)lock.acquire();session.resume();session.wake();mutableFailure.value=null}
    else {session.pause();if(lock.isHeld)lock.release()}
   } catch(e:Exception){if(e is CancellationException)throw e;report(e);if(lock.isHeld)lock.release()}
   finally {if(closed && lock.isHeld)lock.release()}
  }} finally {if(lock.isHeld)lock.release();scope.cancel()}
 }}
 override fun onStart(owner:LifecycleOwner){if(!closed)transitions.trySend(true)}
 override fun onStop(owner:LifecycleOwner){if(!closed)transitions.trySend(false)}
 /** Serialized teardown: an earlier pause cannot win over a later foreground request. */
 override fun close(){closed=true;transitions.trySend(false);transitions.close()}
}
