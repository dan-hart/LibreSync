package io.libresync.notes.sample
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.*
import androidx.compose.material3.*
import androidx.compose.foundation.layout.*
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.*
import libresync.*
import libresync.compose.*
import java.io.File
class MainActivity:ComponentActivity() {
 private var session:Session?=null
 private var resources:SessionLifecycle?=null
 override fun onCreate(savedInstanceState:Bundle?){super.onCreate(savedInstanceState)
  lifecycleScope.launch {try {
   val s=Session.open(SessionConfiguration.notes(File(filesDir,"managed-notes"),android.os.Build.MODEL),AndroidKeystoreStorage(this@MainActivity,"io.libresync.Notes"));session=s
   val resources=SessionLifecycle(this@MainActivity,s);this@MainActivity.resources=resources;lifecycle.addObserver(resources)
   setContent {MaterialTheme {var tab by remember{mutableStateOf(0)};var text by remember{mutableStateOf("")};var incoming by remember{mutableStateOf<List<ManagedRecord>>(emptyList())};var error by remember{mutableStateOf<String?>(null)};val scope=rememberCoroutineScope();val failure by resources.failure.collectAsState()
    LaunchedEffect(s){while(isActive){try {val inbox=s.inbox();withContext(Dispatchers.IO){InboxJournal.save(File(filesDir,"application-inbox.json"),inbox)};s.acknowledge(inbox);incoming=inbox.records}catch(e:Exception){error=e.message};delay(500)}}
    Column {Row {listOf("Notes","Connect","Devices","Status").forEachIndexed{i,label->TextButton(onClick={tab=i}){Text(label)}}};when(tab){0->{OutlinedTextField(text,{text=it},label={Text("Local note")});Button(onClick={scope.launch{try{s.set("records","sample-note",text.toByteArray())}catch(e:Exception){error=e.message}}}){Text("Save local note")};Text("Incoming records and source receipt proofs are saved in one durable app journal before Applied acknowledgement.");incoming.filter{!it.deleted}.forEach{Text("${it.id}: ${it.value.toString(Charsets.UTF_8)}")}};1->Connect(s);2->Devices(s);else->Status(s)};error?.let{Text(it)};failure?.let{Text(it.message)}}}}
  }catch(e:Exception){setContent{Text(e.message?:"Secure storage unavailable")}}}
 }
 override fun onDestroy(){resources?.let{lifecycle.removeObserver(it);it.close()};session?.close();super.onDestroy()}
}
