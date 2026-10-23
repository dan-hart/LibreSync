package io.libresync.notes.sample

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.activity.compose.setContent
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.unit.dp
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.*
import libresync.*
import libresync.compose.*
import java.io.File

class MainActivity : ComponentActivity() {
    private var session: Session? = null
    private var resources: SessionLifecycle? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            NotesTheme {
                Surface(Modifier.fillMaxSize()) {
                    Column(
                        Modifier.fillMaxSize().safeDrawingPadding().padding(24.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically)
                    ) {
                        CircularProgressIndicator()
                        Text("Opening Nearby Sync…", style = MaterialTheme.typography.bodyLarge)
                    }
                }
            }
        }
        lifecycleScope.launch {
            try {
                val s = Session.open(
                    SessionConfiguration.notes(File(filesDir, "managed-notes"), android.os.Build.MODEL),
                    AndroidKeystoreStorage(this@MainActivity, "io.libresync.Notes")
                )
                session = s
                val resources = SessionLifecycle(this@MainActivity, s)
                this@MainActivity.resources = resources
                lifecycle.addObserver(resources)
                setContent {
                    NotesTheme {
                        var tab by remember { mutableStateOf(0) }
                        var text by remember { mutableStateOf("") }
                        var incoming by remember { mutableStateOf<List<ManagedRecord>>(emptyList()) }
                        var error by remember { mutableStateOf<String?>(null) }
                        val scope = rememberCoroutineScope()
                        val failure by resources.failure.collectAsState()
                        LaunchedEffect(s) {
                            while (isActive) {
                                try {
                                    val inbox = s.inbox()
                                    withContext(Dispatchers.IO) {
                                        InboxJournal.save(File(filesDir, "application-inbox.json"), inbox)
                                    }
                                    s.acknowledge(inbox)
                                    incoming = inbox.records
                                } catch (e: Exception) {
                                    error = e.message
                                }
                                delay(500)
                            }
                        }
                        NotesScaffold(tab, { tab = it }) {
                            Column(Modifier.fillMaxSize()) {
                                error?.let { SampleError(it) }
                                failure?.let { SampleError(it.message ?: "Sync needs attention") }
                                Box(Modifier.weight(1f).fillMaxWidth()) {
                                    when (tab) {
                                        0 -> Column(
                                            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
                                            verticalArrangement = Arrangement.spacedBy(12.dp)
                                        ) {
                                            Text("Notes", style = MaterialTheme.typography.headlineSmall)
                                            Text("A local note, shared with your trusted devices.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                            OutlinedTextField(text, { text = it }, label = { Text("Local note") }, modifier = Modifier.fillMaxWidth(), minLines = 3)
                                            Button(onClick = {
                                                scope.launch {
                                                    try {
                                                        s.set("records", "sample-note", text.toByteArray())
                                                    } catch (e: Exception) {
                                                        error = e.message
                                                    }
                                                }
                                            }) { Text("Save local note") }
                                            HorizontalDivider(Modifier.padding(vertical = 8.dp))
                                            Text("Received notes", style = MaterialTheme.typography.titleMedium)
                                            Text("Incoming records and source receipt proofs are saved in one durable app journal before Applied acknowledgement.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                            val records = incoming.filter { !it.deleted }
                                            if (records.isEmpty()) {
                                                Text("No received notes yet", style = MaterialTheme.typography.titleSmall)
                                                Text("Connect another device to start sharing notes on your local network.", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                            }
                                            records.forEach { record ->
                                                OutlinedCard(Modifier.fillMaxWidth()) {
                                                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                                                        Text(record.id, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                                        Text(record.value.toString(Charsets.UTF_8), style = MaterialTheme.typography.bodyLarge)
                                                    }
                                                }
                                            }
                                        }
                                        1 -> Connect(s)
                                        2 -> Devices(s)
                                        else -> Status(s)
                                    }
                                }
                            }
                        }
                    }
                }
            } catch (e: Exception) {
                setContent {
                    NotesTheme {
                        Surface(Modifier.fillMaxSize()) {
                            Column(Modifier.safeDrawingPadding().padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                                Text("Unable to open Notes", style = MaterialTheme.typography.headlineSmall)
                                Text(e.message ?: "Secure storage unavailable", style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.error)
                                Text("Unlock secure storage and reopen the app. Your device identity is kept in secure storage.", style = MaterialTheme.typography.bodyMedium)
                            }
                        }
                    }
                }
            }
        }
    }

    override fun onDestroy() {
        resources?.let { lifecycle.removeObserver(it); it.close() }
        session?.close()
        super.onDestroy()
    }
}

@Composable
private fun NotesTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme(), content = content)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NotesScaffold(selectedTab: Int, onTabSelected: (Int) -> Unit, content: @Composable () -> Unit) {
    Scaffold(
        topBar = {
            Column {
                TopAppBar(title = { Text("LibreSync Notes") })
                SecondaryScrollableTabRow(selectedTabIndex = selectedTab, edgePadding = 12.dp) {
                    listOf("Notes", "Connect", "Devices", "Status").forEachIndexed { index, label ->
                        Tab(selected = selectedTab == index, onClick = { onTabSelected(index) }, text = { Text(label) })
                    }
                }
            }
        }
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) { content() }
    }
}

@Composable
private fun SampleError(message: String) {
    Surface(color = MaterialTheme.colorScheme.errorContainer, contentColor = MaterialTheme.colorScheme.onErrorContainer, modifier = Modifier.fillMaxWidth()) {
        Text(message, Modifier.padding(horizontal = 20.dp, vertical = 12.dp), style = MaterialTheme.typography.bodyMedium)
    }
}
