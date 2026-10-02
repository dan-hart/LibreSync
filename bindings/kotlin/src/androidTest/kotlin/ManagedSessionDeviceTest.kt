package libresync
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.Assert.*
import org.junit.runner.RunWith
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*
import java.io.File
import java.util.UUID
@RunWith(AndroidJUnit4::class)
class ManagedSessionDeviceTest {
 private val context get()=InstrumentationRegistry.getInstrumentation().targetContext
 private fun directory()=File(context.cacheDir,"session-${UUID.randomUUID()}")
 @Test fun typedJniPairingBootstrapInboxDurabilityAndFlowCancellation()=runBlocking {
  val store=AndroidKeystoreStorage(context,"pair-${UUID.randomUUID()}")
  val a=Session.open(SessionConfiguration.notes(directory(),"Android A").copy(advertise=false,listen="127.0.0.1:0"),store)
  val b=Session.open(SessionConfiguration.notes(directory(),"Android B").copy(advertise=false,listen="127.0.0.1:0"),store)
  try {
   a.set("records","one","hello".toByteArray());b.set("records","two","world".toByteArray());a.start();b.start()
   val invite=a.invitation();b.connect(invite.encoded())
   val aId=a.snapshot().identity.device_id;val bId=b.snapshot().identity.device_id
   withTimeout(5000){while(a.bootstrap(bId)==null||b.bootstrap(aId)==null)delay(30)}
   // Each merge changes the other device's incoming candidate. Stop both owned
   // runtimes before capturing consent so neither reviewed preview can change.
   a.pause();b.pause()
   assertEquals(SessionPhase.Paused,a.snapshot().phase);assertEquals(SessionPhase.Paused,b.snapshot().phase)
   assertEquals(1,a.records("records").size);assertEquals(1,b.records("records").size)
   val aPreview=requireNotNull(a.bootstrap(bId));val bPreview=requireNotNull(b.bootstrap(aId))
   a.resolve(aPreview,BootstrapDecision.Merge);b.resolve(bPreview,BootstrapDecision.Merge)
   val aAddress=a.resume();val bAddress=b.resume()
   suspend fun refreshHint(source:Session,target:Session,address:String){
    // The loopback fixture has no discovery provider. Supply only the real
    // resumed listener and its own public metadata through the discovery API.
    val txt=source.command("platform_advertisement").jsonObject.getValue("txt").jsonObject
    fun field(key:String)=txt.getValue(key).jsonPrimitive.content
    val ad=Advertisement(field("name"),field("kind"),field("role"),field("app_name"),field("schema").toUInt(),field("contract"))
    val peer=NearbyDevice(source.snapshot().identity,listOf(address),ad)
    target.command("ingest_discovery",mapOf("peer" to Session.json.encodeToJsonElement(peer)))
   }
   refreshHint(a,b,aAddress);refreshHint(b,a,bAddress)
   withTimeout(5000){while(b.records("records").size!=2||b.inbox().receipts.isEmpty())delay(30)}
   val inbox=b.inbox();assertEquals(2,inbox.records.size);assertTrue(inbox.receipts.isNotEmpty());assertTrue(inbox.receipts.values.all{it.proof.size==32})
   val source=a.snapshot().identity.device_id;val checkpoint=inbox.receipts.getValue(source)
   assertFalse(a.snapshot().peers.first().applied.proof.contentEquals(checkpoint.proof))
   val obstruction=File(context.cacheDir,"obstruction-${UUID.randomUUID()}").apply{writeText("file")}
   try {InboxJournal.save(File(obstruction,"failed.json"),inbox);fail("App save should fail")}catch(_:java.io.IOException){}
   assertFalse(a.snapshot().peers.first().applied.proof.contentEquals(checkpoint.proof))
   val appFile=File(directory(),"inbox.json");InboxJournal.save(appFile,inbox)
   val saved=Session.json.decodeFromString(ApplicationInbox.serializer(),appFile.readText());assertTrue(saved.receipts.values.zip(inbox.receipts.values).all{it.first.proof.contentEquals(it.second.proof)})
   b.acknowledge(saved)
   withTimeout(5000){while(!a.snapshot().peers.first().applied.proof.contentEquals(checkpoint.proof))delay(30)}
   assertEquals(checkpoint.sequence,a.snapshot().peers.first().applied.sequence)
   val observer=launch{a.events().collect{}}
   delay(50);observer.cancelAndJoin();assertEquals(SessionPhase.Running,a.snapshot().phase)
   val receipt=ManagedReceipt("epoch",ULong.MAX_VALUE,ByteArray(32){it.toByte()});val decoded=Session.json.decodeFromString(ManagedReceipt.serializer(),Session.json.encodeToString(ManagedReceipt.serializer(),receipt));assertEquals(ULong.MAX_VALUE,decoded.sequence);assertTrue(receipt.proof.contentEquals(decoded.proof))
   assertEquals(2,LibreSyncNative.libresync_abi_version());assertNotNull(LibreSyncNative.libresync_generate_app_key())
  }finally{a.closeAndJoin();b.closeAndJoin()}
 }
 @Test fun independentFlowsAndRapidLifecycleTransitions()=runBlocking {
  val session=Session.open(SessionConfiguration.notes(directory(),"Lifecycle test").copy(advertise=false,listen="127.0.0.1:0"),AndroidKeystoreStorage(context,"lifecycle-${UUID.randomUUID()}"))
  val lifecycle=SessionLifecycle(context,session)
  val owner=object:androidx.lifecycle.LifecycleOwner {override val lifecycle:androidx.lifecycle.Lifecycle=androidx.lifecycle.LifecycleRegistry(this)}
  try {
   repeat(20){lifecycle.onStop(owner);lifecycle.onStart(owner)}
   withTimeout(5000){while(session.snapshot().phase!=SessionPhase.Running)delay(10)}
   delay(100);assertEquals(SessionPhase.Running,session.snapshot().phase)
   val first=async(start=CoroutineStart.UNDISPATCHED){withTimeout(5000){session.events().first{it is SessionEvent.Changed}}}
   val second=async(start=CoroutineStart.UNDISPATCHED){withTimeout(5000){session.events().first{it is SessionEvent.Changed}}}
   delay(100);session.set("records","observed","shared".toByteArray());first.await();second.await()
   assertEquals(SessionPhase.Running,session.snapshot().phase)
   lifecycle.close()
   withTimeout(5000){while(session.snapshot().phase!=SessionPhase.Paused)delay(10)}
   assertNull(lifecycle.failure.value)
  } finally {lifecycle.close();session.closeAndJoin()}
 }
 @Test fun secureStoreDistinguishesAbsentDeniedAndCorruptFilesAndActualApi37Permission() {
  assertTrue(android.os.Build.VERSION.SDK_INT>=37);assertTrue(context.applicationInfo.targetSdkVersion>=37)
  val granted=context.checkSelfPermission(LocalNetworkPermission.permission)==android.content.pm.PackageManager.PERMISSION_GRANTED
  assertEquals(if(granted)PermissionState.Allowed else PermissionState.Denied,LocalNetworkPermission.state(context))
  val service="errors-${UUID.randomUUID()}";val store=AndroidKeystoreStorage(context,service)
  assertNull(store.load("missing"));store.store("test","owned-test".toByteArray())
  fun digest(s:String)=java.security.MessageDigest.getInstance("SHA-256").digest(s.toByteArray()).joinToString(""){"%02x".format(it)}
  val file=File(context.noBackupFilesDir,"libresync-keys/${digest(service)}/${digest("test")}.cipher")
  android.system.Os.chmod(file.path,0)
  try {try{store.load("test");fail("Denied read must not become absence")}catch(e:android.system.ErrnoException){assertEquals(android.system.OsConstants.EACCES,e.errno)}}finally{android.system.Os.chmod(file.path,384)}
  file.writeBytes(byteArrayOf(1,2,3));try{store.load("test");fail("Corrupt read must not become absence")}catch(_:IllegalStateException){}
  store.delete("test");assertNull(store.load("test"))
 }
 @Test fun closeThenJoinReleasesLeaseForImmediateReopen()=runBlocking {
  val dir=directory();val store=AndroidKeystoreStorage(context,"close-${UUID.randomUUID()}");val config=SessionConfiguration.notes(dir,"Close test").copy(advertise=false)
  val session=Session.open(config,store);session.start();session.close();session.closeAndJoin()
  val reopened=Session.open(config,store);reopened.closeAndJoin()
 }
 @Test fun realAndroidKeystoreParallelInstancesPreserveBothCiphertexts() {
  val service="parallel-${UUID.randomUUID()}";val a=AndroidKeystoreStorage(context,service);val b=AndroidKeystoreStorage(context,service)
  val gate=java.util.concurrent.CyclicBarrier(2)
  val executor=java.util.concurrent.Executors.newFixedThreadPool(2)
  try {
   val first=executor.submit{gate.await();a.store("one","first".toByteArray())}
   val second=executor.submit{gate.await();b.store("two","second".toByteArray())}
   first.get();second.get();assertArrayEquals("first".toByteArray(),a.load("one"));assertArrayEquals("second".toByteArray(),a.load("two"));assertArrayEquals("first".toByteArray(),b.load("one"));assertArrayEquals("second".toByteArray(),b.load("two"))
  }finally{executor.shutdown();a.delete("one");a.delete("two")}
 }
}
