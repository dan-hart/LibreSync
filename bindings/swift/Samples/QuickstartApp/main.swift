import SwiftUI
import LibreSync

// Copy into a new SwiftUI app target and add the SDK package. Include Info.plist
// from this folder. This exact notes contract pairs with the always-on catalog.
@main
struct NotesQuickstart:App {
    @StateObject private var host=NotesHost()
    @Environment(\.scenePhase) private var phase
    var body:some Scene {WindowGroup {Group {if let model=host.model {TabView{NotesEditor(model:model).tabItem{Text("Notes")};LibreSyncConnectView(model:model).tabItem{Text("Connect")};LibreSyncDevicesView(model:model).tabItem{Text("Devices")};LibreSyncStatusView(model:model).tabItem{Text("Status")}}}else{Text(host.error ?? "Opening secure storage…")}}.task{await host.open()}.onChange(of:phase){value in if value == .active{host.model?.start()}else if value == .background{host.model?.stop()}}}}
}
@MainActor
private final class NotesHost:ObservableObject {
    @Published var model:LibreSyncSessionModel?
    @Published var error:String?
    func open()async {guard model==nil else{return};do{let directory=FileManager.default.urls(for:.applicationSupportDirectory,in:.userDomainMask)[0].appendingPathComponent("LibreSyncNotes");let session=try await LibreSyncSession.open(.notes(stateDirectory:directory,displayName:"My Apple device"),keys:LibreSyncKeychainStore(service:"io.libresync.Notes"));model=LibreSyncSessionModel(session:session);model?.start()}catch{self.error=error.localizedDescription}}
}
private struct NotesEditor:View {
    @ObservedObject var model:LibreSyncSessionModel
    @State var text=""
    @State var incoming:[LibreSyncRecord]=[]
    @State var error:String?
    var body:some View {VStack{TextField("Local note",text:$text);Button("Save local note"){Task{do{try await model.session.set(adapter:"records",id:"sample-note",value:Data(text.utf8))}catch{self.error=error.localizedDescription}}};Text("Incoming records and receipt proofs are saved together before Applied acknowledgement.");ForEach(incoming.filter{!$0.deleted},id:\.id){record in Text("\(record.id): \(String(data:record.value,encoding:.utf8) ?? "Binary record")")};if let error{Text(error)}}.padding().task {do{while !Task.isCancelled{let inbox=try await model.session.inbox();let file=FileManager.default.urls(for:.applicationSupportDirectory,in:.userDomainMask)[0].appendingPathComponent("LibreSyncNotes/application-inbox.json");try await Task.detached{try LibreSyncInboxJournal.save(inbox,to:file)}.value;try await model.session.acknowledge(inbox);incoming=inbox.records;try await Task.sleep(nanoseconds:500_000_000)}}catch{if !Task.isCancelled{self.error=error.localizedDescription}}}}
}
