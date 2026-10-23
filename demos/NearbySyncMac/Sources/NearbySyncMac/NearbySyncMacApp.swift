import SwiftUI
import LibreSync

private let accent = Color(red: 1, green: 79.0 / 255, blue: 0)

@main
struct NearbySyncMacApp: App {
    @NSApplicationDelegateAdaptor(DemoAppDelegate.self) private var delegate
    @StateObject private var model = DemoModel()
    var body: some Scene {
        WindowGroup("Nearby Notes") {
            DemoShell(model: model)
                .accentColor(accent)
                .task { delegate.model = model; await model.start() }
        }.commands { CommandGroup(replacing: .newItem) {} }
    }
}

private enum Destination: String, CaseIterable { case notes = "Notes", nearby = "Nearby Sync" }

private struct DemoShell: View {
    @ObservedObject var model: DemoModel
    @State private var destination: Destination? = .notes
    @State private var selectedNote: String?
    @State private var draft = DemoNoteDraft()
    @State private var renaming = false
    @State private var newName = ""

    var body: some View {
        NavigationView {
            List(selection: $destination) {
                Section("LIBRESYNC") {
                    Label("Notes", systemImage: "note.text").tag(Destination.notes)
                    Label("Nearby Sync", systemImage: "personalhotspot").tag(Destination.nearby)
                }
            }.listStyle(.sidebar)
                .frame(minWidth: 185, idealWidth: 205)
                .safeAreaInset(edge: .bottom) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(model.sync?.localDisplayName ?? "Nearby Notes").font(.subheadline.weight(.medium))
                        Text("Profile: \(model.profile?.name ?? "Invalid")").font(.caption).foregroundColor(.secondary)
                        Text("On your network. On your devices.").font(.caption2).foregroundColor(.secondary)
                    }.frame(maxWidth: .infinity, alignment: .leading).padding(16)
                }
            VStack(spacing: 0) {
                if let error = model.error {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundColor(.orange)
                        Text(error).font(.callout).textSelection(.enabled)
                        Spacer()
                        Button("Dismiss") { model.error = nil }
                    }.padding().background(Color.orange.opacity(0.08))
                    Divider()
                }
                if destination == .nearby {
                    if let sync = model.sync {
                        ScrollView { LibreSyncNearbySyncView(model: sync, onRename: {
                            newName = sync.localDisplayName; renaming = true
                        }).padding(28).frame(maxWidth: 800) }
                    } else { unavailable }
                } else { notesWorkspace }
            }.frame(minWidth: 620, minHeight: 520)
        }
        .frame(minWidth: 860, minHeight: 600)
        .navigationTitle("Nearby Notes — \(model.profile?.name ?? "Invalid profile")")
        .onChange(of: model.notes) { notes in
            draft.receive(notes.first { $0.id == draft.id })
        }
        .sheet(isPresented: $renaming) {
            VStack(alignment: .leading, spacing: 18) {
                Text("Rename this device").font(.title2.bold())
                Text("Choose a name you will recognize on your other devices.").foregroundColor(.secondary)
                TextField("Device name", text: $newName)
                HStack {
                    Spacer()
                    Button("Cancel") { renaming = false }.keyboardShortcut(.cancelAction)
                    Button("Save Name") { model.rename(newName); renaming = false }
                        .keyboardShortcut(.defaultAction)
                        .disabled(newName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || newName.utf8.count > 128)
                }
            }.padding(28).frame(width: 420)
        }
    }

    private var unavailable: some View {
        VStack(spacing: 15) {
            if model.busy { ProgressView(); Text("Opening your private sync session…") }
            else { Text("Sync could not start").font(.title2); Button("Try Again") { Task { await model.start() } } }
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var notesWorkspace: some View {
        HSplitView {
            VStack(spacing: 0) {
                HStack {
                    Text("Notes").font(.headline)
                    Spacer()
                    Button { select(nil) } label: { Image(systemName: "square.and.pencil") }
                        .help("New note").accessibilityLabel("New note")
                }.padding(16)
                Divider()
                if model.notes.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("A little space for your thoughts.").font(.headline)
                        Text("Write a note, then connect another device in Nearby Sync.").font(.callout).foregroundColor(.secondary)
                    }.padding(20).frame(maxHeight: .infinity, alignment: .top)
                } else {
                    List(selection: $selectedNote) {
                        ForEach(model.notes) { note in
                            VStack(alignment: .leading, spacing: 5) {
                                Text(note.title.isEmpty ? "Untitled note" : note.title).font(.headline).lineLimit(1)
                                Text(note.body.isEmpty ? "No additional text" : note.body).foregroundColor(.secondary).lineLimit(2)
                                Text(note.updatedAt, style: .date).font(.caption2).foregroundColor(.secondary)
                            }.padding(.vertical, 6).tag(note.id)
                        }
                    }.listStyle(.inset)
                        .onChange(of: selectedNote) { id in load(id) }
                }
            }.frame(minWidth: 215, idealWidth: 250, maxWidth: 310)
            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    Text(selectedNote == nil ? "New note" : "Edit note").font(.subheadline).foregroundColor(.secondary)
                    Spacer()
                    if model.busy { ProgressView().controlSize(.small) }
                    Button("Save Note") { model.save(id: draft.id, title: draft.title, body: draft.body) }
                        .keyboardShortcut("s", modifiers: .command)
                        .disabled(model.sync == nil || model.busy || (draft.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && draft.body.isEmpty))
                }
                if draft.sourceChanged {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("This note changed on another device. Saving will publish your current draft.")
                        Button("Load latest version") {
                            draft.load(model.notes.first { $0.id == draft.id })
                        }
                    }.font(.callout).padding(12).background(Color.orange.opacity(0.1))
                }
                TextField("Title", text: $draft.title).textFieldStyle(.plain).font(.title.bold())
                Divider()
                TextEditor(text: $draft.body).font(.body).accessibilityLabel("Note text")
                HStack(spacing: 5) {
                    Image(systemName: "lock.shield")
                    Text("Notes stay on your devices. Save to share with connected devices.")
                }.font(.caption).foregroundColor(.secondary)
            }.padding(24).frame(minWidth: 360)
        }
    }
    private func select(_ id: String?) { selectedNote = id; load(id) }
    private func load(_ id: String?) {
        let note = model.notes.first { $0.id == id }
        draft.load(note)
    }
}
