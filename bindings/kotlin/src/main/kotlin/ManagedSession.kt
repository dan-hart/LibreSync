package libresync

import android.util.Base64
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.*
import kotlinx.serialization.json.*
import kotlinx.serialization.descriptors.*
import kotlinx.serialization.encoding.*
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

object BytesAsBase64:KSerializer<ByteArray> {
 override val descriptor=PrimitiveSerialDescriptor("Base64Bytes",PrimitiveKind.STRING)
 override fun serialize(encoder:Encoder,value:ByteArray)=encoder.encodeString(Base64.encodeToString(value,Base64.NO_WRAP))
 override fun deserialize(decoder:Decoder)=Base64.decode(decoder.decodeString(),Base64.NO_WRAP)
}
@Serializable enum class SessionErrorCode { Closed,Cancelled,InvalidOperation,InvalidConfiguration,InvalidAdapter,InvalidRecord,IncompatibleSchema,StaleBootstrap,GroupConflict,PeerRevoked,StorageUnavailable,StorageCommitUncertain,Busy,OperationFailed }
@Serializable data class SessionFailure(val code:SessionErrorCode,val message:String)
class SessionException(val failure:SessionFailure):Exception(failure.message)
@Serializable enum class PermissionState {Allowed,Denied,Unknown}
@Serializable data class AdapterDescriptor(val id:String,val namespace:String,val schema:String="logical-records-v1",val transactional:Boolean=true)
@Serializable data class AppManifest(val app_id:String,val display_name:String,val schema_version:UInt=1u,val adapters:List<AdapterDescriptor>)
@Serializable data class SessionConfiguration(val policy:String,val state_dir:String,val display_name:String,val device_kind:String="phone",val role:String="device",val manifest:AppManifest?=null,val listen:String?=null,val advertise:Boolean=true,val authenticated_io_timeout_ms:Long=60_000) {
 init {require(authenticated_io_timeout_ms in 1..300_000){"Authenticated IO timeout must be greater than zero and at most five minutes"}}
 companion object {fun notes(stateDirectory:File,displayName:String)=SessionConfiguration("notes",stateDirectory.path,displayName)}
}
@Serializable data class Identity(val device_id:String,val user_id:String,val app_id:String)
@Serializable data class DeviceMetadata(val display_name:String,val device_kind:String,val role:String,val manifest:AppManifest)
@Serializable data class LogicalClock(val counter:ULong,val device_id:String)
@Serializable data class ManagedRecord(val adapter:String,val id:String,@Serializable(with=BytesAsBase64::class) val value:ByteArray,val deleted:Boolean,val clock:LogicalClock)
/** Source-issued proof is opaque. Save it verbatim with the app's inbox transaction. */
@Serializable data class ManagedReceipt(val epoch:String,val sequence:ULong,@Serializable(with=BytesAsBase64::class) val proof:ByteArray)
@Serializable private data class InboxAcknowledgement(val revision:ULong,val receipts:Map<String,ManagedReceipt>)
@Serializable data class ApplicationInbox(val revision:ULong,val records:List<ManagedRecord>,val receipts:Map<String,ManagedReceipt>)
@Serializable enum class SessionPhase {Stopped,Failed,Running,Paused}
@Serializable enum class PeerState {Waiting,Exchanging,Stored,UpToDate,NeedsMerge,NeedsRepair,Paused}
@Serializable data class PeerSnapshot(val identity:Identity,val metadata:DeviceMetadata,val fingerprint:String,val revoked:Boolean=false,val pending:ULong,val stored:ManagedReceipt,val applied:ManagedReceipt,val transmitted:ManagedReceipt,val received:ManagedReceipt,val processed:ManagedReceipt,val state:PeerState)
@Serializable enum class DiagnosticAction { None,CheckPermissions,Retry,RepairPeer,ReviewMerge,GrantPermission,CheckSecureStorage,ReviewCompatibility,ReviewRecords,ResolveGroupConflict,ReviewConfiguration,Wait }
@Serializable data class SessionDiagnostic(val peer:String?,val message:String,val action:DiagnosticAction,val evidence:JsonElement)
@Serializable data class SessionSnapshot(val identity:Identity,val phase:SessionPhase,val local_revision:ULong,val peers:List<PeerSnapshot>,val diagnostics:List<JsonElement>)
@Serializable data class PairingDescriptor(val version:UInt,val invitation_id:String,val inviter_fingerprint:String,val expires_at:ULong)
@Serializable data class PairingCredential(val version:UInt,val invitation_id:String,val inviter_fingerprint:String,val metadata:DeviceMetadata,val expires_at:ULong,val secret:String)
@Serializable data class SessionInvitation(val invitation:PairingCredential,val identity:Identity,val addresses:List<String>) {fun encoded()=Session.json.encodeToString(this)}
@Serializable data class Enrollment(val invitation_id:String,val identity:Identity,val metadata:DeviceMetadata,val fingerprint:String)
@Serializable data class Advertisement(val display_name:String,val device_kind:String,val role:String,val app_display_name:String,val schema_version:UInt,val contract_digest:String)
@Serializable data class NearbyDevice(val identity:Identity,val addresses:List<String>,val advertisement:Advertisement?=null,val invitation:PairingDescriptor?=null)
@Serializable data class ExportBatch(val checkpoint:ManagedReceipt,val records:List<ManagedRecord>,val full:Boolean)
@Serializable data class BootstrapPreview(val token:String,val peer:String,val fingerprint:String,val local_revision:ULong,val batch:ExportBatch)
@Serializable enum class BootstrapDecision {Merge,Cancel}
@Serializable data class RecoverySnapshot(val id:String,val revision:ULong,val records:List<ManagedRecord>)
sealed interface SessionEvent { data object Changed:SessionEvent;data class Diagnostic(val value:SessionDiagnostic):SessionEvent }

/** Session owns encrypted storage. Acknowledge only after saving the entire
 * ApplicationInbox records AND receipts durably in one application transaction. */
class Session private constructor(id:Long):AutoCloseable {
 private val handle=AtomicLong(id)
 private val closeMutex=Any()
 private var closing:java.util.concurrent.CompletableFuture<JsonElement>?=null
 private fun closeFuture():java.util.concurrent.CompletableFuture<JsonElement> = synchronized(closeMutex){
  closing?.let{return@synchronized it}
  val future=java.util.concurrent.CompletableFuture<JsonElement>();closing=future
  val id=handle.getAndSet(0)
  if(id==0L)future.complete(JsonNull) else cancellation.execute{try{future.complete(value(ManagedNative.close(id)))}catch(e:Throwable){future.completeExceptionally(e)}}
  future
 }
 companion object {
  internal val json=Json { encodeDefaults=true;explicitNulls=false }
  private val workers=Executors.newFixedThreadPool(8){r->Thread(r,"LibreSync worker").apply{isDaemon=true}}
  private val cancellation=Executors.newSingleThreadExecutor{r->Thread(r,"LibreSync cancellation").apply{isDaemon=true}}
  private fun value(response:String):JsonElement {val e=json.parseToJsonElement(response).jsonObject; if(e["abi"]?.jsonPrimitive?.int!=1)throw SessionException(SessionFailure(SessionErrorCode.IncompatibleSchema,"Unsupported managed ABI"));e["error"]?.let{throw SessionException(json.decodeFromJsonElement(it))};return e["value"] ?: JsonNull}
  suspend fun open(configuration:SessionConfiguration,keys:LibreSyncKeyStorage):Session=suspendCancellableCoroutine {continuation->
   workers.execute {if(!continuation.isActive)return@execute;try{val session=Session(value(ManagedNative.open(json.encodeToString(configuration),keys)).jsonPrimitive.long);continuation.resume(session,onCancellation={_,s,_->s.close()})}catch(e:Throwable){if(continuation.isActive)continuation.resumeWithException(e)}}
  }
 }
 suspend fun command(op:String,fields:Map<String,JsonElement> = emptyMap()):JsonElement {
  val id=handle.get();if(id==0L)throw SessionException(SessionFailure(SessionErrorCode.Closed,"Session closed"))
  val payload=JsonObject(fields+ ("op" to JsonPrimitive(op))).toString()
  return suspendCancellableCoroutine {continuation->
   workers.execute {if(!continuation.isActive)return@execute;try {val result=value(ManagedNative.call(id,payload));if(continuation.isActive)continuation.resume(result)}catch(e:Throwable){if(continuation.isActive)continuation.resumeWithException(e)}}
   continuation.invokeOnCancellation {if(op in setOf("connect","connect_code","repair","recover_pairing"))cancellation.execute {ManagedNative.call(id,"{\"op\":\"cancel\"}")}}
  }
 }
 /** Immediate handle invalidation; cleanup is scheduled off the calling thread. */
 override fun close(){closeFuture()}
 suspend fun closeAndJoin(){val future=closeFuture();withContext(Dispatchers.IO){try{future.get()}catch(e:java.util.concurrent.ExecutionException){throw (e.cause?:e)}}}
 suspend fun cancel(){command("cancel")}
 suspend fun start()=command("start").jsonPrimitive.content
 suspend fun shutdown(){command("shutdown")}
 suspend fun pause(){command("pause")}
 suspend fun resume()=command("resume").jsonPrimitive.content
 suspend fun wake(){command("wake")}
 suspend fun snapshot():SessionSnapshot=json.decodeFromJsonElement(command("snapshot"))
 suspend fun set(adapter:String,id:String,value:ByteArray){command("set",mapOf("adapter" to JsonPrimitive(adapter),"id" to JsonPrimitive(id),"value" to JsonPrimitive(Base64.encodeToString(value,Base64.NO_WRAP))))}
 suspend fun delete(adapter:String,id:String){command("delete",mapOf("adapter" to JsonPrimitive(adapter),"id" to JsonPrimitive(id)))}
 suspend fun records(adapter:String):List<ManagedRecord> = json.decodeFromJsonElement(command("records",mapOf("adapter" to JsonPrimitive(adapter))))
 suspend fun inbox():ApplicationInbox=json.decodeFromJsonElement(command("inbox"))
 suspend fun acknowledge(inbox:ApplicationInbox){withContext(Dispatchers.Default){command("acknowledge_inbox",mapOf("inbox" to json.encodeToJsonElement(InboxAcknowledgement(inbox.revision,inbox.receipts))))}}
 suspend fun importRecords(records:List<ManagedRecord>){command("import",mapOf("records" to json.encodeToJsonElement(records)))}
 suspend fun invitation(code:Boolean=false,ttlMillis:Long=120_000):SessionInvitation=json.decodeFromJsonElement(command("invitation",mapOf("code" to JsonPrimitive(code),"ttl_ms" to JsonPrimitive(ttlMillis))))
 suspend fun closePairing(){command("close_pairing")}
 suspend fun connect(encoded:String):Enrollment=json.decodeFromJsonElement(command("connect",mapOf("invitation" to JsonPrimitive(encoded))))
 suspend fun connect(peer:NearbyDevice,code:String):Enrollment=json.decodeFromJsonElement(command("connect_code",mapOf("peer" to json.encodeToJsonElement(peer),"code" to JsonPrimitive(code))))
 suspend fun repair(peer:String,invitation:String):Enrollment=json.decodeFromJsonElement(command("repair",mapOf("peer" to JsonPrimitive(peer),"invitation" to JsonPrimitive(invitation))))
 suspend fun remove(peer:String){command("remove_peer",mapOf("peer" to JsonPrimitive(peer)))}
 suspend fun pause(peer:String){command("pause_peer",mapOf("peer" to JsonPrimitive(peer)))}
 suspend fun resume(peer:String){command("resume_peer",mapOf("peer" to JsonPrimitive(peer)))}
 suspend fun nearby():List<NearbyDevice> = json.decodeFromJsonElement(command("discovery"))
 suspend fun bootstrap(peer:String):BootstrapPreview?=json.decodeFromJsonElement(command("bootstrap_preview",mapOf("peer" to JsonPrimitive(peer))))
 suspend fun resolve(preview:BootstrapPreview,decision:BootstrapDecision){command("resolve_bootstrap",mapOf("preview" to json.encodeToJsonElement(preview),"decision" to json.encodeToJsonElement(decision)))}
 suspend fun recovery():List<RecoverySnapshot> = json.decodeFromJsonElement(command("recovery"))
 suspend fun pruneRecovery(retain:Int){command("prune_recovery",mapOf("retain" to JsonPrimitive(retain)))}
 suspend fun recoverPairing(){command("recover_pairing")}
 suspend fun pendingPairings():List<Enrollment> = json.decodeFromJsonElement(command("pending_pairings"))
 suspend fun reportPermission(state:PermissionState){command("evidence",mapOf("platform" to JsonPrimitive("Android"),"permission" to JsonPrimitive("LocalNetwork"),"state" to json.encodeToJsonElement(state)))}
 fun events():Flow<SessionEvent> = flow {
  val subscription=withContext(NonCancellable){command("subscribe")}
  try{while(currentCoroutineContext().isActive){val events=command("events",mapOf("timeout_ms" to JsonPrimitive(200),"subscription" to subscription)).jsonArray;for(event in events){if(event is JsonPrimitive && event.content=="Changed")emit(SessionEvent.Changed)else{val diagnostic=event.jsonObject["Diagnostic"]?:throw SerializationException("Unknown session event");emit(SessionEvent.Diagnostic(json.decodeFromJsonElement(diagnostic)))}}}}
  finally{withContext(NonCancellable){try{command("unsubscribe",mapOf("subscription" to subscription))}catch(_:SessionException){}}}
 }.flowOn(Dispatchers.IO)
}
