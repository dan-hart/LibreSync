package libresync.compose
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.compose.material3.*
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import com.google.zxing.*
import com.google.zxing.common.HybridBinarizer
import com.google.zxing.qrcode.QRCodeWriter
import kotlinx.coroutines.*
import libresync.*
import kotlinx.serialization.json.*

object OfflineQR {
 fun image(payload:String,size:Int=512):Bitmap {
  require(payload.toByteArray().size<=16*1024)
  val matrix=QRCodeWriter().encode(payload,BarcodeFormat.QR_CODE,size,size)
  return Bitmap.createBitmap(size,size,Bitmap.Config.ARGB_8888).apply{for(y in 0 until size)for(x in 0 until size)setPixel(x,y,if(matrix[x,y])android.graphics.Color.BLACK else android.graphics.Color.WHITE)}
 }
 fun decode(bitmap:Bitmap):String?=try{
  val pixels=IntArray(bitmap.width*bitmap.height);bitmap.getPixels(pixels,0,bitmap.width,0,0,bitmap.width,bitmap.height)
  MultiFormatReader().decode(BinaryBitmap(HybridBinarizer(RGBLuminanceSource(bitmap.width,bitmap.height,pixels)))).text.takeIf{it.toByteArray().size<=16*1024}
 }catch(_:Exception){null}
}
@Composable fun Connect(session:Session,modifier:Modifier=Modifier) {
 val scope=rememberCoroutineScope();val context=LocalContext.current
 var nearby by remember{mutableStateOf<List<NearbyDevice>>(emptyList())};var invitation by remember{mutableStateOf<SessionInvitation?>(null)}
 var now by remember{mutableStateOf(System.currentTimeMillis()/1000)}
 LaunchedEffect(Unit){while(isActive){now=System.currentTimeMillis()/1000;delay(1000)}}
 var encoded by remember{mutableStateOf("")};var code by remember{mutableStateOf("")};var selected by remember{mutableStateOf<NearbyDevice?>(null)}
 var contract by remember{mutableStateOf<String?>(null)}
 var confirmInvitation by remember{mutableStateOf(false)};var error by remember{mutableStateOf<String?>(null)}
 var permission by remember{mutableStateOf(LocalNetworkPermission.state(context))}
 val request=rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()){allowed->permission=if(allowed)PermissionState.Allowed else PermissionState.Denied;scope.launch{try{session.reportPermission(permission)}catch(e:Exception){error=e.message}}}
 val camera=rememberLauncherForActivityResult(ActivityResultContracts.TakePicturePreview()){bitmap->if(bitmap!=null){encoded=OfflineQR.decode(bitmap)?:"";if(encoded.isNotEmpty())confirmInvitation=true else error="Could not read QR. Try a clearer image or nearby six-digit code."}}
 val photo=rememberLauncherForActivityResult(ActivityResultContracts.GetContent()){uri->if(uri!=null){scope.launch(Dispatchers.IO){val bitmap=context.contentResolver.openInputStream(uri)?.use{BitmapFactory.decodeStream(it)};val payload=bitmap?.let{OfflineQR.decode(it)};withContext(Dispatchers.Main){if(payload!=null){encoded=payload;confirmInvitation=true}else error="Image contains no readable invitation QR."}}}}
 LaunchedEffect(session){while(isActive){try{nearby=session.nearby();contract=session.command("platform_advertisement").jsonObject["txt"]?.jsonObject?.get("contract")?.jsonPrimitive?.content}catch(e:Exception){error=e.message};delay(500)}}
 Column(modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)) {
  Text("Connect a device",style=MaterialTheme.typography.headlineSmall)
  Text("Devices sync directly on your local network. Connecting grants access to this app’s sync group.")
  if(LocalNetworkPermission.requestRequired(context)){Button(onClick={request.launch(LocalNetworkPermission.permission)}){Text("Allow Local Network")}}
  Text("Local Network: $permission")
  Row {Button(onClick={scope.launch{try{invitation=session.invitation()}catch(e:Exception){error=e.message}}}){Text("Show QR")};Button(onClick={scope.launch{try{invitation=session.invitation(true)}catch(e:Exception){error=e.message}}}){Text("Show code")}}
  invitation?.let {i->if(i.invitation.expires_at<=now.toULong()){Text("Invitation expired. Renew using Show QR or Show code.")}else{if(i.invitation.secret.length==6)Text(i.invitation.secret,style=MaterialTheme.typography.displaySmall)else{val qr=remember(i){runCatching{OfflineQR.image(i.encoded())}.getOrNull()};if(qr!=null)Image(qr.asImageBitmap(),"Private pairing QR",Modifier.size(256.dp))else Text("Invitation is too large for QR. Use a nearby six-digit code.")};Text("Expires in ${i.invitation.expires_at-now.toULong()} seconds; keep the QR and code private.")}}
  Row {Button(onClick={camera.launch(null)}){Text("Scan using camera")};Button(onClick={photo.launch("image/*")}){Text("Read QR image")}}
  OutlinedTextField(encoded,{encoded=it},label={Text("Paste invitation as an alternative")})
  Button(onClick={confirmInvitation=true},enabled=encoded.isNotEmpty()){Text("Connect invitation")}
  Text("Nearby devices",style=MaterialTheme.typography.titleMedium)
  if(nearby.isEmpty())Text("No device found yet. Check Wi-Fi and Local Network access. Empty discovery does not establish permission denial.")
  nearby.forEach{peer->Text(peer.advertisement?.display_name?:"Nearby device");if(peer.invitation?.version==2u && peer.advertisement?.contract_digest==contract)Button(onClick={selected=peer}){Text("Enter code")}else Text("Update both devices or open pairing on that device.")}
  error?.let{Text(it,color=MaterialTheme.colorScheme.error)}
 }
 if(confirmInvitation)AlertDialog(onDismissRequest={confirmInvitation=false},title={Text("Connect this app’s sync group?")},text={Text("Connect ${invitationName(encoded)}? The invitation and certificate will be authenticated before this device is enrolled.")},confirmButton={TextButton(onClick={confirmInvitation=false;scope.launch{try{session.connect(encoded)}catch(e:Exception){error=e.message}}}){Text("Connect")}},dismissButton={TextButton(onClick={confirmInvitation=false}){Text("Cancel")}})
 selected?.let{peer->AlertDialog(onDismissRequest={selected=null},title={Text(peer.advertisement?.display_name?:"Nearby device")},text={Column{Text("Enter this device’s six-digit code to grant access to this app’s sync group.");OutlinedTextField(code,{code=it},label={Text("Six-digit code")})}},confirmButton={TextButton(onClick={selected=null;scope.launch{try{session.connect(peer,code)}catch(e:Exception){error=e.message}}}){Text("Connect")}},dismissButton={TextButton(onClick={selected=null}){Text("Cancel")}})}
}
@Composable fun Devices(session:Session,modifier:Modifier=Modifier) {
 val scope=rememberCoroutineScope();var snapshot by remember{mutableStateOf<SessionSnapshot?>(null)};var preview by remember{mutableStateOf<BootstrapPreview?>(null)}
 var removing by remember{mutableStateOf<PeerSnapshot?>(null)};var repairing by remember{mutableStateOf<PeerSnapshot?>(null)};var invitation by remember{mutableStateOf("")};var error by remember{mutableStateOf<String?>(null)}
 LaunchedEffect(session){while(isActive){try{snapshot=session.snapshot()}catch(e:Exception){error=e.message};delay(500)}}
 Column(modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(12.dp)){Text("Devices",style=MaterialTheme.typography.headlineSmall);snapshot?.peers?.forEach{peer->Text(peer.metadata.display_name);Text(if(peer.revoked)"Removed — stored data retained" else "${peer.state}; pending ${peer.pending}");Row{if(peer.state==PeerState.NeedsMerge)TextButton(onClick={scope.launch{try{preview=session.bootstrap(peer.identity.device_id)}catch(e:Exception){error=e.message}}}){Text("Review combine")};if(!peer.revoked)TextButton(onClick={scope.launch{try{if(peer.state==PeerState.Paused)session.resume(peer.identity.device_id)else session.pause(peer.identity.device_id)}catch(e:Exception){error=e.message}}}){Text(if(peer.state==PeerState.Paused)"Resume" else "Pause")};TextButton(onClick={repairing=peer}){Text("Repair")};TextButton(onClick={removing=peer}){Text("Remove")}}};error?.let{Text(it)}}
 preview?.let{p->AlertDialog(onDismissRequest={},title={Text("Combine existing data?")},text={Text("Incoming records: ${p.batch.records.size}. A recovery copy is retained before the reviewed app merge policy combines records.")},confirmButton={TextButton(onClick={scope.launch{try{session.resolve(p,BootstrapDecision.Merge);preview=null}catch(e:Exception){error=e.message}}}){Text("Combine")}},dismissButton={TextButton(onClick={scope.launch{try{session.resolve(p,BootstrapDecision.Cancel);preview=null}catch(e:Exception){error=e.message}}}){Text("Cancel")}})}
 removing?.let{p->AlertDialog(onDismissRequest={removing=null},title={Text("Remove device trust?")},text={Text("Local data stays on this device. Reconnecting requires a fresh invitation and explicit repair.")},confirmButton={TextButton(onClick={removing=null;scope.launch{try{session.remove(p.identity.device_id)}catch(e:Exception){error=e.message}}}){Text("Remove")}},dismissButton={TextButton(onClick={removing=null}){Text("Cancel")}})}
 repairing?.let{p->AlertDialog(onDismissRequest={repairing=null},title={Text("Repair ${p.metadata.display_name}")},text={OutlinedTextField(invitation,{invitation=it},label={Text("Fresh replacement invitation")})},confirmButton={TextButton(onClick={repairing=null;scope.launch{try{session.repair(p.identity.device_id,invitation)}catch(e:Exception){error=e.message}}}){Text("Repair trust")}},dismissButton={TextButton(onClick={repairing=null}){Text("Cancel")}})}
}
@Composable fun Status(session:Session,modifier:Modifier=Modifier) {
 val scope=rememberCoroutineScope();var snapshot by remember{mutableStateOf<SessionSnapshot?>(null)};var diagnostics by remember{mutableStateOf<List<SessionDiagnostic>>(emptyList())};var error by remember{mutableStateOf<String?>(null)}
 LaunchedEffect(session){try{snapshot=session.snapshot();session.events().collect{event->snapshot=session.snapshot();if(event is SessionEvent.Diagnostic)diagnostics=(diagnostics+event.value).takeLast(20)}}catch(e:Exception){if(e is CancellationException)throw e;error=e.message}}
 Column(modifier.padding(16.dp)){error?.let{Text(it,color=MaterialTheme.colorScheme.error)};Text("Sync: ${snapshot?.phase?:"Stopped"}");Button(onClick={scope.launch{try{session.resume();session.wake()}catch(e:Exception){error=e.message}}}){Text("Retry / reconnect")};diagnostics.forEach{d->Text(d.message);Text(when(d.action){DiagnosticAction.GrantPermission,DiagnosticAction.CheckPermissions->"Allow Local Network in Settings, check Wi-Fi, then retry.";DiagnosticAction.RepairPeer->"Get a fresh invitation and repair trust in Devices.";DiagnosticAction.ReviewMerge->"Review Combine or Cancel in Devices.";DiagnosticAction.CheckSecureStorage->"Unlock secure storage and retry. Do not regenerate identity.";else->d.action.name})}}
}

private fun invitationName(encoded:String):String=try{if(encoded.toByteArray().size>16*1024)"invited device" else Json.parseToJsonElement(encoded).jsonObject["invitation"]?.jsonObject?.get("metadata")?.jsonObject?.get("display_name")?.jsonPrimitive?.content?.take(128)?:"invited device"}catch(_:Exception){"invited device"}
