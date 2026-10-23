import SwiftUI
import UIKit
import LibreSync

@main
struct NearbyNotesIOSApp: App {
    // iOS injects launch arguments for debugging; profiles are a desktop CLI feature.
    @StateObject private var model = DemoModel(arguments: [])
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            PhoneNotesView(model: model)
                .tint(Color(red: 1, green: 79.0 / 255, blue: 0))
                .task {
                    await model.setActive(scenePhase == .active)
                    await model.start()
                }
                .onChange(of: scenePhase) { _, phase in
                    // Keep discovery through transient inactive privacy/QR sheets;
                    // only actual backgrounding pauses networking.
                    guard phase != .inactive else { return }
                    let backgroundTask = phase == .background
                        ? UIApplication.shared.beginBackgroundTask(withName: "Pause nearby sync", expirationHandler: nil)
                        : .invalid
                    Task {
                        await model.setActive(phase == .active)
                        if backgroundTask != .invalid { UIApplication.shared.endBackgroundTask(backgroundTask) }
                    }
                }
        }
    }
}

private struct NoteDraft: Identifiable {
    let id: String
    var title: String
    var body: String
    init(_ note: DemoNote? = nil) {
        id = note?.id ?? UUID().uuidString
        title = note?.title ?? ""
        body = note?.body ?? ""
    }
}

private struct PhoneNotesView: View {
    @ObservedObject var model: DemoModel
    @State private var draft: NoteDraft?
    @State private var renaming = false
    @State private var deviceName = ""

    var body: some View {
        TabView {
            NavigationStack {
                Group {
                    if model.sync == nil { unavailable }
                    else if model.notes.isEmpty {
                        ContentUnavailableView {
                            Label("Your notes, nearby", systemImage: "note.text")
                        } description: {
                            Text("Write a note, then connect your Mac in Nearby Sync.")
                        } actions: {
                            Button("Write a note") { draft = NoteDraft() }.disabled(model.busy)
                        }
                    } else {
                        List(model.notes) { note in
                            Button { draft = NoteDraft(note) } label: {
                                VStack(alignment: .leading, spacing: 5) {
                                    Text(note.title.isEmpty ? "Untitled note" : note.title)
                                        .font(.headline).foregroundColor(.primary).lineLimit(2)
                                    Text(note.body.isEmpty ? "No additional text" : note.body)
                                        .foregroundColor(.secondary).lineLimit(2)
                                    Text(note.updatedAt, style: .date).font(.caption).foregroundColor(.secondary)
                                }.padding(.vertical, 6).frame(maxWidth: .infinity, alignment: .leading)
                            }.buttonStyle(.plain).disabled(model.busy)
                        }.listStyle(.insetGrouped)
                    }
                }
                .navigationTitle("Notes")
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { draft = NoteDraft() } label: { Image(systemName: "square.and.pencil") }
                            .accessibilityLabel("New note").disabled(model.sync == nil || model.busy)
                    }
                }
            }.tabItem { Label("Notes", systemImage: "note.text") }
            NavigationStack {
                Group {
                    if let sync = model.sync {
                        LibreSyncNearbySyncView(model: sync, onRename: {
                            deviceName = sync.localDisplayName; renaming = true
                        }).disabled(model.busy)
                    } else { unavailable }
                }.navigationTitle("Nearby Sync")
            }.tabItem { Label("Nearby Sync", systemImage: "personalhotspot") }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            if let error = model.error {
                HStack(alignment: .top) {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                    Text(error).font(.callout).textSelection(.enabled)
                    Button { model.error = nil } label: { Image(systemName: "xmark.circle.fill") }
                        .accessibilityLabel("Dismiss error")
                }.padding().background(.regularMaterial)
            }
        }
        .sheet(item: $draft) { draft in NoteEditor(model: model, draft: draft) }
        .sheet(isPresented: $renaming) {
            NavigationStack {
                Form {
                    Section {
                        TextField("Device name", text: $deviceName).textInputAutocapitalization(.words)
                            .submitLabel(.done).onSubmit(saveName)
                    } header: {
                        Text("Device name")
                    } footer: {
                        Text("Choose a name you will recognize on your other devices.")
                    }
                }.navigationTitle("Rename this device").navigationBarTitleDisplayMode(.inline)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) { Button("Cancel") { renaming = false } }
                        ToolbarItem(placement: .confirmationAction) { Button("Save", action: saveName).disabled(!validName || model.busy) }
                    }
            }.presentationDetents([.medium, .large])
        }
    }

    private var validName: Bool {
        let name = deviceName.trimmingCharacters(in: .whitespacesAndNewlines)
        return !name.isEmpty && name.utf8.count <= 128 &&
            name.unicodeScalars.allSatisfy { !CharacterSet.controlCharacters.contains($0) }
    }
    private func saveName() {
        guard validName, !model.busy else { return }
        model.rename(deviceName); renaming = false
    }
    private var unavailable: some View {
        VStack(spacing: 16) {
            if model.busy { ProgressView("Opening your private sync session…") }
            else {
                ContentUnavailableView("Sync could not start", systemImage: "wifi.exclamationmark",
                                       description: Text("Check the error above and try again."))
                Button("Try Again") { Task { await model.start() } }
            }
        }.padding().frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct NoteEditor: View {
    @ObservedObject var model: DemoModel
    @Environment(\.dismiss) private var dismiss
    @State var draft: NoteDraft
    @State private var original: DemoNote?
    @State private var saving = false
    @State private var sourceChanged = false

    private var canSave: Bool {
        model.sync != nil && !model.busy &&
            (!draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !draft.body.isEmpty)
    }
    var body: some View {
        NavigationStack {
            Form {
                if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                if sourceChanged {
                    Section {
                        Text("This note changed on another device. Saving will publish your current draft.")
                        Button("Load latest version") { loadLatest() }
                    }
                }
                Section {
                    TextField("Title", text: $draft.title).font(.title2.bold()).submitLabel(.next)
                        .accessibilityLabel("Note title")
                    TextEditor(text: $draft.body).frame(minHeight: 280).accessibilityLabel("Note text")
                        .lineSpacing(4)
                }
                Section {} footer: {
                    Label("Save to share with connected devices.", systemImage: "lock.shield")
                }
            }.navigationTitle(original == nil ? "New note" : "Edit note").navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() }.disabled(saving) }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            guard canSave else { return }
                            saving = true; model.save(id: draft.id, title: draft.title, body: draft.body)
                        }.keyboardShortcut("s", modifiers: .command).disabled(!canSave || saving)
                    }
                }
                .onAppear { original = model.notes.first { $0.id == draft.id } }
                .onChange(of: model.notes) { _, notes in
                    let latest = notes.first { $0.id == draft.id }
                    if saving, latest?.title == draft.title.trimmingCharacters(in: .whitespacesAndNewlines), latest?.body == draft.body {
                        dismiss()
                    } else if latest != original { sourceChanged = true }
                }
                .onChange(of: model.busy) { _, busy in
                    if saving && !busy { saving = false }
                }
        }.interactiveDismissDisabled(saving)
    }
    private func loadLatest() {
        guard let latest = model.notes.first(where: { $0.id == draft.id }) else { return }
        draft = NoteDraft(latest); original = latest; sourceChanged = false
    }
}
