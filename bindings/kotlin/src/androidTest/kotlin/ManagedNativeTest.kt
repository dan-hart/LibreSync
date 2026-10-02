package libresync
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Test
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.json.JSONObject
@RunWith(AndroidJUnit4::class)
class ManagedNativeTest {
 @Test fun realJniStoresRecordsAndDoesNotRegenerateLockedIdentity() {
  val dir=java.io.File(InstrumentationRegistry.getInstrumentation().targetContext.cacheDir,"native-${java.util.UUID.randomUUID()}")
  val config=JSONObject().put("policy","notes").put("state_dir",dir.path).put("display_name","Android native").put("device_kind","phone").toString()
  val keys=object:LibreSyncKeyStorage { val items=mutableMapOf<String,ByteArray>();override fun load(keyId:String)=items[keyId];override fun store(keyId:String,data:ByteArray){items[keyId]=data};override fun delete(keyId:String){items.remove(keyId)} }
  val opened=JSONObject(ManagedNative.open(config,keys));assertTrue(opened.toString(),opened.getBoolean("ok"));val h=opened.getLong("value")
  assertEquals("Busy",JSONObject(ManagedNative.open(config,keys)).getJSONObject("error").getString("code"))
  assertTrue(JSONObject(ManagedNative.call(h,"{\"op\":\"set\",\"adapter\":\"records\",\"id\":\"one\",\"value\":\"aGk=\"}")).getBoolean("ok"))
  assertEquals("aGk=",JSONObject(ManagedNative.call(h,"{\"op\":\"records\",\"adapter\":\"records\"}")).getJSONArray("value").getJSONObject(0).getString("value"))
  ManagedNative.close(h);ManagedNative.close(h)
  assertEquals("Closed",JSONObject(ManagedNative.call(h,"{\"op\":\"snapshot\"}")).getJSONObject("error").getString("code"))
  val locked=object:LibreSyncKeyStorage {override fun load(keyId:String):ByteArray?=throw IllegalStateException("locked");override fun store(keyId:String,data:ByteArray){fail("identity regenerated")};override fun delete(keyId:String){fail("deleted identity")} }
  assertFalse(JSONObject(ManagedNative.open(config,locked)).getBoolean("ok"))
 }
}
