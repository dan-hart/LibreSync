package libresync
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.system.Os
import android.system.OsConstants
import android.system.ErrnoException
import java.io.FileInputStream
import java.io.File
import java.io.FileOutputStream
import java.security.KeyStore
import java.security.MessageDigest
import java.util.concurrent.ConcurrentHashMap
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** AES-GCM ciphertext files in app-private no-backup storage. A kernel file
 * lease plus process-wide mutex serializes alias creation across instances and
 * processes; every read reloads its file. Secure errors never mean "absent". */
class AndroidKeystoreStorage(context:Context,service:String):LibreSyncKeyStorage {
 companion object {private val locks=ConcurrentHashMap<String,Any>();private fun digest(s:String)=MessageDigest.getInstance("SHA-256").digest(s.toByteArray()).joinToString(""){"%02x".format(it)}}
 private val namespace=digest(service)
 private val directory=File(context.applicationContext.noBackupFilesDir,"libresync-keys/$namespace").apply{check(mkdirs() || isDirectory)}
 private val alias="libresync.$namespace"
 private val mutex=locks.computeIfAbsent(directory.canonicalPath){Any()}
 private fun <T> locked(body:()->T):T=synchronized(mutex){FileOutputStream(File(directory,"store.lock"),true).channel.use{channel->channel.lock().use{body()}}}
 private fun key(create:Boolean):SecretKey {
  val keyStore=KeyStore.getInstance("AndroidKeyStore").apply{load(null)}
  (keyStore.getKey(alias,null) as? SecretKey)?.let{return it}
  check(create && directory.listFiles()?.none{it.name.endsWith(".cipher")}==true){"Secure key missing while ciphertext exists; repair secure storage"}
  return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES,"AndroidKeyStore").apply{init(KeyGenParameterSpec.Builder(alias,KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())}.generateKey()
 }
 override fun store(keyId:String,data:ByteArray):Unit=locked {
  val cipher=Cipher.getInstance("AES/GCM/NoPadding");cipher.init(Cipher.ENCRYPT_MODE,key(true));cipher.updateAAD(keyId.toByteArray())
  durableReplace(File(directory,digest(keyId)+".cipher"),cipher.iv+cipher.doFinal(data))
 }
 override fun load(keyId:String):ByteArray?=locked {
  val file=File(directory,digest(keyId)+".cipher")
  val descriptor=try{Os.open(file.path,OsConstants.O_RDONLY,0)}catch(e:ErrnoException){if(e.errno==OsConstants.ENOENT)return@locked null else throw e}
  val payload=FileInputStream(descriptor).use{input->val output=java.io.ByteArrayOutputStream();val buffer=ByteArray(4096);while(true){val count=input.read(buffer);if(count<0)break;check(output.size()+count<=1024*1024){"Secure item exceeds limit"};output.write(buffer,0,count)};output.toByteArray()}
  check(payload.size>=28){"Corrupt encrypted secure store"};val cipher=Cipher.getInstance("AES/GCM/NoPadding");cipher.init(Cipher.DECRYPT_MODE,key(false),GCMParameterSpec(128,payload.copyOfRange(0,12)));cipher.updateAAD(keyId.toByteArray());cipher.doFinal(payload.copyOfRange(12,payload.size))
 }
 override fun delete(keyId:String):Unit=locked {val file=File(directory,digest(keyId)+".cipher");try{Os.remove(file.path)}catch(e:ErrnoException){if(e.errno!=OsConstants.ENOENT)throw e};syncDirectory(directory)}
}
internal fun syncDirectory(directory:File){val fd=Os.open(directory.path,OsConstants.O_RDONLY,0);try{Os.fsync(fd)}finally{Os.close(fd)}}
internal fun durableReplace(file:File,data:ByteArray){val parent=requireNotNull(file.parentFile);if(!parent.mkdirs() && !parent.isDirectory)throw java.io.IOException("Application inbox directory unavailable");val temp=File(parent,".${file.name}.${java.util.UUID.randomUUID()}.tmp");try{FileOutputStream(temp).use{it.write(data);it.fd.sync()};Os.rename(temp.path,file.path);syncDirectory(parent)}finally{temp.delete()}}
/** Example coherent records+receipts transaction. Use your own app database
 * transaction in production, then acknowledge exactly this saved inbox. */
object InboxJournal { fun save(file:File,inbox:ApplicationInbox){durableReplace(file,Session.json.encodeToString(ApplicationInbox.serializer(),inbox).toByteArray())} }
