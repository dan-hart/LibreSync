package libresync.compose
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.compose.material3.*
import androidx.compose.foundation.Image
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.font.FontFamily
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
 ManagedPage(modifier) {
  Text("Connect a device",style=MaterialTheme.typography.headlineSmall)
  SupportingText("Devices sync directly on your local network. Connecting grants access to this app’s sync group.")
  SectionTitle("This device")
  if(LocalNetworkPermission.requestRequired(context)){Button(onClick={request.launch(LocalNetworkPermission.permission)}){Text("Allow Local Network")}}
  SupportingText(when(permission) { PermissionState.Allowed -> "Local network access allowed"; PermissionState.Denied -> "Local network access denied. Allow access in Android Settings to discover devices."; PermissionState.Unknown -> "Local network access has not been confirmed." })
  FlowRow(horizontalArrangement=Arrangement.spacedBy(8.dp),verticalArrangement=Arrangement.spacedBy(4.dp)) {
   Button(onClick={scope.launch{try{invitation=session.invitation()}catch(e:Exception){error=e.message}}}){Text("Show QR") }
   OutlinedButton(onClick={scope.launch{try{invitation=session.invitation(true)}catch(e:Exception){error=e.message}}}){Text("Show code") }
  }
  invitation?.let { i ->
   Surface(color=MaterialTheme.colorScheme.surfaceContainer,shape=MaterialTheme.shapes.large,modifier=Modifier.fillMaxWidth()) {
    Column(Modifier.padding(20.dp),verticalArrangement=Arrangement.spacedBy(12.dp),horizontalAlignment=Alignment.CenterHorizontally) {
     if(i.invitation.expires_at<=now.toULong()) {
      Text("Invitation expired",style=MaterialTheme.typography.titleMedium)
      SupportingText("Tap Show QR or Show code to create a new invitation.")
     } else {
      if(i.invitation.secret.length==6) Text(i.invitation.secret,style=MaterialTheme.typography.headlineMedium,fontFamily=FontFamily.Monospace)
      else {
       val qr=remember(i){runCatching{OfflineQR.image(i.encoded())}.getOrNull()}
       if(qr!=null) Image(qr.asImageBitmap(),"Private pairing QR",Modifier.widthIn(max=256.dp).fillMaxWidth().aspectRatio(1f))
       else SupportingText("Invitation is too large for QR. Use a nearby six-digit code.")
      }
      Text("Expires in ${i.invitation.expires_at-now.toULong()} seconds",style=MaterialTheme.typography.labelLarge)
      SupportingText("Keep this QR and code private. Only share with a device you trust.")
     }
    }
   }
  }
  SectionTitle("Connect another device")
  FlowRow(horizontalArrangement=Arrangement.spacedBy(8.dp),verticalArrangement=Arrangement.spacedBy(4.dp)) {
   OutlinedButton(onClick={camera.launch(null)}){Text("Scan using camera")}
   TextButton(onClick={photo.launch("image/*")}){Text("Read QR image")}
  }
  OutlinedTextField(encoded,{encoded=it},label={Text("Paste invitation")},modifier=Modifier.fillMaxWidth(),minLines=2,maxLines=4)
  Button(onClick={confirmInvitation=true},enabled=encoded.isNotEmpty()){Text("Connect invitation")}
  SectionTitle("Nearby devices")
  if(nearby.isEmpty()) EmptyMessage("Looking for devices", "Open Nearby Sync on the other device and connect both devices to the same Wi-Fi network. If nothing appears, check local network access.")
  nearby.forEach { peer ->
   OutlinedCard(Modifier.fillMaxWidth()) {
    Column(Modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(8.dp)) {
     Text(peer.advertisement?.display_name?.takeIf{it.isNotBlank()}?:"Nearby device",style=MaterialTheme.typography.titleMedium)
     if(peer.invitation?.version==2u && peer.advertisement?.contract_digest==contract) {
      val invitationCurrent=peer.invitation?.expires_at?.let{it>now.toULong()}==true
      SupportingText(if(invitationCurrent) "Ready to connect with a six-digit code" else "Invitation expired. Ask this device to show a new code.")
      TextButton(onClick={selected=peer},enabled=invitationCurrent){Text("Enter code")}
     } else SupportingText("Update both devices or open pairing on that device.")
    }
   }
  }
  error?.let{ErrorMessage(it)}
 }
 if(confirmInvitation)AlertDialog(onDismissRequest={confirmInvitation=false},title={Text("Connect this app’s sync group?")},text={Text("Connect ${invitationName(encoded)}? The invitation and certificate will be authenticated before this device is enrolled.")},confirmButton={TextButton(onClick={confirmInvitation=false;scope.launch{try{session.connect(encoded)}catch(e:Exception){error=e.message}}}){Text("Connect")}},dismissButton={TextButton(onClick={confirmInvitation=false}){Text("Cancel")}})
 selected?.let { peer ->
  val invitationCurrent=peer.invitation?.expires_at?.let{it>now.toULong()}==true
  AlertDialog(
   onDismissRequest={selected=null},
   title={Text(peer.advertisement?.display_name?:"Nearby device")},
   text={
    Column(Modifier.heightIn(max=360.dp).verticalScroll(rememberScrollState()),verticalArrangement=Arrangement.spacedBy(12.dp)) {
     Text(if(invitationCurrent) "Enter this device’s six-digit code to grant access to this app’s sync group." else "This invitation has expired. Cancel and ask the device to show a new code.")
     OutlinedTextField(code,{code=it},label={Text("Six-digit code")},modifier=Modifier.fillMaxWidth(),singleLine=true,keyboardOptions=KeyboardOptions(keyboardType=KeyboardType.Number))
    }
   },
   confirmButton={TextButton(enabled=invitationCurrent,onClick={selected=null;scope.launch{try{session.connect(peer,code)}catch(e:Exception){error=e.message}}}){Text("Connect")}},
   dismissButton={TextButton(onClick={selected=null}){Text("Cancel")}}
  )
 }

}
@Composable fun Devices(session:Session,modifier:Modifier=Modifier) {
 val scope=rememberCoroutineScope();var snapshot by remember{mutableStateOf<SessionSnapshot?>(null)};var preview by remember{mutableStateOf<BootstrapPreview?>(null)}
 var removing by remember{mutableStateOf<PeerSnapshot?>(null)};var repairing by remember{mutableStateOf<PeerSnapshot?>(null)};var invitation by remember{mutableStateOf("")};var error by remember{mutableStateOf<String?>(null)}
 LaunchedEffect(session){while(isActive){try{snapshot=session.snapshot()}catch(e:Exception){error=e.message};delay(500)}}
 ManagedPage(modifier) {
  Text("Devices",style=MaterialTheme.typography.headlineSmall)
  SupportingText("Manage the devices trusted to sync this app’s data.")
  if(snapshot==null && error==null) LoadingMessage("Loading devices…")
  if(snapshot?.peers?.isEmpty()==true) EmptyMessage("No connected devices", "Use Connect to pair another device on your local network.")
  snapshot?.peers?.forEach { peer ->
   var menuExpanded by remember(peer.identity.device_id){mutableStateOf(false)}
   OutlinedCard(Modifier.fillMaxWidth()) {
    Column(Modifier.padding(16.dp),verticalArrangement=Arrangement.spacedBy(8.dp)) {
     Text(peer.metadata.display_name.takeIf{it.isNotBlank()}?:"Connected device",style=MaterialTheme.typography.titleMedium)
     Text(if(peer.revoked) "Removed · stored data retained" else peerStatus(peer.state),style=MaterialTheme.typography.bodyMedium,color=MaterialTheme.colorScheme.onSurfaceVariant)
     Box {
       TextButton(onClick={menuExpanded=true}){Text("Manage")}
       DropdownMenu(expanded=menuExpanded,onDismissRequest={menuExpanded=false}) {
        if(!peer.revoked) DropdownMenuItem(text={Text(if(peer.state==PeerState.Paused)"Resume sync" else "Pause sync")},onClick={menuExpanded=false;scope.launch{try{if(peer.state==PeerState.Paused)session.resume(peer.identity.device_id)else session.pause(peer.identity.device_id)}catch(e:Exception){error=e.message}}})
        DropdownMenuItem(text={Text("Repair trust")},onClick={menuExpanded=false;repairing=peer})
        DropdownMenuItem(text={Text("Remove device",color=MaterialTheme.colorScheme.error)},onClick={menuExpanded=false;removing=peer})
       }
     }
     if(!peer.revoked && peer.pending>0uL) SupportingText("${peer.pending} pending changes")
     if(peer.state==PeerState.Stored && !peer.revoked) SupportingText("Stored safely by the other device. Waiting for its app to apply the data.")
     if(peer.state==PeerState.NeedsMerge) TextButton(onClick={scope.launch{try{preview=session.bootstrap(peer.identity.device_id)}catch(e:Exception){error=e.message}}}){Text("Review combine")}
    }
   }
  }
  error?.let{ErrorMessage(it)}
 }

 preview?.let{p->AlertDialog(onDismissRequest={},title={Text("Combine existing data?")},text={Text("Incoming records: ${p.batch.records.size}. A recovery copy is retained before the reviewed app merge policy combines records.")},confirmButton={TextButton(onClick={scope.launch{try{session.resolve(p,BootstrapDecision.Merge);preview=null}catch(e:Exception){error=e.message}}}){Text("Combine")}},dismissButton={TextButton(onClick={scope.launch{try{session.resolve(p,BootstrapDecision.Cancel);preview=null}catch(e:Exception){error=e.message}}}){Text("Cancel")}})}
 removing?.let{p->AlertDialog(onDismissRequest={removing=null},title={Text("Remove device trust?")},text={Text("Local data stays on this device. Reconnecting requires a fresh invitation and explicit repair.")},confirmButton={TextButton(onClick={removing=null;scope.launch{try{session.remove(p.identity.device_id)}catch(e:Exception){error=e.message}}}){Text("Remove")}},dismissButton={TextButton(onClick={removing=null}){Text("Cancel")}})}
 repairing?.let{p->AlertDialog(onDismissRequest={repairing=null},title={Text("Repair ${p.metadata.display_name}")},text={Column(Modifier.heightIn(max=360.dp).verticalScroll(rememberScrollState()),verticalArrangement=Arrangement.spacedBy(12.dp)){Text("Use a fresh invitation from this device to repair its trust.");OutlinedTextField(invitation,{invitation=it},label={Text("Fresh replacement invitation")},modifier=Modifier.fillMaxWidth(),minLines=2,maxLines=4)}},confirmButton={TextButton(onClick={repairing=null;scope.launch{try{session.repair(p.identity.device_id,invitation)}catch(e:Exception){error=e.message}}}){Text("Repair trust")}},dismissButton={TextButton(onClick={repairing=null}){Text("Cancel")}})}
}
@Composable fun Status(session:Session,modifier:Modifier=Modifier) {
 val scope=rememberCoroutineScope();var snapshot by remember{mutableStateOf<SessionSnapshot?>(null)};var diagnostics by remember{mutableStateOf<List<SessionDiagnostic>>(emptyList())};var error by remember{mutableStateOf<String?>(null)}
 LaunchedEffect(session){try{snapshot=session.snapshot();session.events().collect{event->snapshot=session.snapshot();if(event is SessionEvent.Diagnostic)diagnostics=(diagnostics+event.value).takeLast(20)}}catch(e:Exception){if(e is CancellationException)throw e;error=e.message}}
 ManagedPage(modifier) {
  Text("Sync status",style=MaterialTheme.typography.headlineSmall)
  error?.let{ErrorMessage(it)}
  if(snapshot==null && error==null) LoadingMessage("Loading sync status…")
  snapshot?.let { current ->
   Surface(color=MaterialTheme.colorScheme.surfaceContainer,shape=MaterialTheme.shapes.large,modifier=Modifier.fillMaxWidth()) {
    Column(Modifier.padding(20.dp),verticalArrangement=Arrangement.spacedBy(8.dp)) {
     Text(when(current.phase){SessionPhase.Running->"Sync is running";SessionPhase.Paused->"Sync is paused";SessionPhase.Failed->"Sync needs attention";SessionPhase.Stopped->"Sync is stopped"},style=MaterialTheme.typography.titleLarge)
     SupportingText("Devices exchange data directly on your local network.")
    }
   }
  }
  Button(onClick={scope.launch{try{session.resume();session.wake()}catch(e:Exception){error=e.message}}}){Text("Retry / reconnect")}
  SectionTitle("Activity & help")
  if(diagnostics.isEmpty()) SupportingText("No diagnostics to show. Sync issues and suggested next steps appear here.")
  diagnostics.forEach { d ->
   Column(verticalArrangement=Arrangement.spacedBy(4.dp)) {
    Text(d.message,style=MaterialTheme.typography.bodyLarge)
    SupportingText(when(d.action){DiagnosticAction.GrantPermission,DiagnosticAction.CheckPermissions->"Allow Local Network in Settings, check Wi-Fi, then retry.";DiagnosticAction.RepairPeer->"Get a fresh invitation and repair trust in Devices.";DiagnosticAction.ReviewMerge->"Review Combine or Cancel in Devices.";DiagnosticAction.CheckSecureStorage->"Unlock secure storage and retry. Do not regenerate identity.";else->d.action.name})
   }
   HorizontalDivider()
  }
 }

}

private fun invitationName(encoded:String):String=try{if(encoded.toByteArray().size>16*1024)"invited device" else Json.parseToJsonElement(encoded).jsonObject["invitation"]?.jsonObject?.get("metadata")?.jsonObject?.get("display_name")?.jsonPrimitive?.content?.take(128)?:"invited device"}catch(_:Exception){"invited device"}

@Composable private fun ManagedPage(modifier:Modifier,content:@Composable ColumnScope.()->Unit) {
 Column(modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),verticalArrangement=Arrangement.spacedBy(12.dp),content=content)
}
@Composable private fun SectionTitle(text:String) {
 Text(text,Modifier.padding(top=12.dp),style=MaterialTheme.typography.titleMedium)
}
@Composable private fun SupportingText(text:String) {
 Text(text,style=MaterialTheme.typography.bodyMedium,color=MaterialTheme.colorScheme.onSurfaceVariant)
}
@Composable private fun EmptyMessage(title:String,message:String) {
 Column(Modifier.fillMaxWidth().padding(vertical=12.dp),verticalArrangement=Arrangement.spacedBy(4.dp)) {
  Text(title,style=MaterialTheme.typography.titleMedium)
  SupportingText(message)
 }
}
@Composable private fun LoadingMessage(message:String) {
 Row(verticalAlignment=Alignment.CenterVertically,horizontalArrangement=Arrangement.spacedBy(12.dp)) {
  CircularProgressIndicator(Modifier.size(24.dp),strokeWidth=2.dp)
  SupportingText(message)
 }
}
@Composable private fun ErrorMessage(message:String) {
 Surface(color=MaterialTheme.colorScheme.errorContainer,contentColor=MaterialTheme.colorScheme.onErrorContainer,shape=MaterialTheme.shapes.medium,modifier=Modifier.fillMaxWidth()) {
  Text(message,Modifier.padding(16.dp),style=MaterialTheme.typography.bodyMedium)
 }
}
private fun peerStatus(state:PeerState):String=when(state) {
 PeerState.Waiting->"Waiting for device"
 PeerState.Exchanging->"Syncing changes"
 PeerState.Stored->"Stored · waiting to apply"
 PeerState.UpToDate->"Up to date · applied"
 PeerState.NeedsMerge->"Review existing data"
 PeerState.NeedsRepair->"Trust needs repair"
 PeerState.Paused->"Sync paused"
}
