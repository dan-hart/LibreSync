import SwiftUI

/// Optional native UI. The host owns session lifetime and application inbox persistence.
public struct LibreSyncNearbySyncView: View {
  @ObservedObject private var model: LibreSyncSessionModel
  private let onRename: (() -> Void)?
  @State private var addingDevice = false

  public init(model: LibreSyncSessionModel, onRename: (() -> Void)? = nil) {
    self.model = model
    self.onRename = onRename
  }

  public var body: some View {
    platformForm
      .sheet(isPresented: $addingDevice, onDismiss: { model.endPairing() }) {
        NearbySyncAddDeviceView(model: model) { addingDevice = false }
      }
  }

  @ViewBuilder private var platformForm: some View {
    #if os(iOS)
      settingsForm
    #else
      ScrollView {
        VStack(alignment: .leading, spacing: 24) {
          VStack(alignment: .leading, spacing: 8) {
            macSectionLabel("This device")
            GroupBox {
              deviceIdentity.padding(8).frame(maxWidth: .infinity, alignment: .leading)
            }
            Text("Sync directly between your devices on the same local network.")
              .font(.caption).foregroundColor(.secondary)
          }
          VStack(alignment: .leading, spacing: 8) {
            macSectionLabel("Your devices")
            GroupBox {
              VStack(alignment: .leading, spacing: 16) {
                deviceContents
                Button(action: addDevice) {
                  Label("Add Device", systemImage: "plus")
                }.buttonStyle(.borderedProminent).disabled(model.isBusy)
              }.padding(8).frame(maxWidth: .infinity, alignment: .leading)
            }
          }
          if let error = model.errorMessage {
            GroupBox {
              VStack(alignment: .leading, spacing: 10) {
                attentionContents(error)
              }.padding(8).frame(maxWidth: .infinity, alignment: .leading)
            }
          }
          GroupBox {
            connectionHelp.padding(8).frame(maxWidth: .infinity, alignment: .leading)
          }
        }
        .frame(maxWidth: 620, alignment: .leading)
        .padding(24)
        .frame(maxWidth: .infinity, alignment: .top)
      }
    #endif
  }

  private func macSectionLabel(_ title: String) -> some View {
    Text(title).font(.subheadline.weight(.semibold)).foregroundColor(.secondary)
  }

  private var settingsForm: some View {
    Form {
      Section {
        deviceIdentity
      } header: {
        Text("This device")
      } footer: {
        Text("Sync directly between your devices on the same local network.")
      }
      Section {
        deviceContents
        Button(action: addDevice) {
          Label("Add Device", systemImage: "plus.circle.fill")
            .frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
        }.disabled(model.isBusy)
      } header: {
        Text("Your devices")
      }
      if let error = model.errorMessage {
        Section {
          attentionContents(error)
        }
      }
      Section {
        connectionHelp
      }
    }
  }

  private var deviceIdentity: some View {
    HStack(alignment: .top, spacing: 12) {
      Image(systemName: localDeviceIcon)
        .font(.title2).foregroundStyle(.tint).frame(width: 32)
        .accessibilityHidden(true)
      VStack(alignment: .leading, spacing: 5) {
        Text(model.localDisplayName).font(.headline).fixedSize(horizontal: false, vertical: true)
        Text(model.phaseDescription).font(.subheadline).foregroundColor(.secondary)
          .fixedSize(horizontal: false, vertical: true)
      }
      Spacer(minLength: 8)
      if let onRename {
        Button("Rename", action: onRename).disabled(model.isBusy)
      }
    }.padding(.vertical, 6)
  }

  @ViewBuilder private var deviceContents: some View {
    if model.snapshot?.peers.isEmpty ?? true {
      VStack(alignment: .leading, spacing: 8) {
        Text("Your devices belong together").font(.headline)
        Text("Add another device to keep this app’s data in sync. Open Nearby Sync on both devices to get started.")
          .foregroundColor(.secondary).fixedSize(horizontal: false, vertical: true)
      }.padding(.vertical, 8)
    } else {
      LibreSyncDevicesView(model: model)
    }
  }

  @ViewBuilder private func attentionContents(_ error: String) -> some View {
    Label("Sync needs attention", systemImage: "exclamationmark.triangle").font(.headline)
    Text(error).textSelection(.enabled)
    Button("Retry Nearby Sync") { model.wake() }.disabled(model.isBusy)
  }

  private var connectionHelp: some View {
    DisclosureGroup("Connection help and details") {
      VStack(alignment: .leading, spacing: 12) {
        Text("Keep both devices on the same local network and open this app on each device. Device names help you choose; the private code or invitation authenticates the connection.")
        Text(permissionHelp)
        Button("Check Local Network access") { model.requestPermission() }
        if let technicalError = model.error {
          Text(technicalError).font(.caption).textSelection(.enabled)
        }
        ForEach(Array((model.snapshot?.diagnostics ?? []).enumerated()), id: \.offset) {
          _, event in
          if case .diagnostic(let diagnostic) = event {
            Text(diagnostic.message).font(.caption).textSelection(.enabled)
          }
        }
      }.foregroundColor(.secondary).padding(.vertical, 8)
    }
  }

  private var localDeviceIcon: String {
    #if os(iOS)
      return "iphone"
    #else
      return "desktopcomputer"
    #endif
  }

  private func addDevice() {
    model.beginPairing()
    addingDevice = model.isPairing
  }

  private var permissionHelp: String {
    switch model.permission {
    case .allowed: return "Local Network access is available."
    case .denied: return "Allow Local Network access for this app in system Settings, then retry."
    case .unknown:
      return
        "Local Network access hasn’t been confirmed. An empty device list can also mean the other device is offline."
    }
  }
}

private struct NearbySyncAddDeviceView: View {
  @ObservedObject var model: LibreSyncSessionModel
  let dismiss: () -> Void
  @State private var selectedID: String?
  @State private var code = ""
  @State private var encoded = ""
  @State private var confirmInvitation = false
  #if os(iOS)
    @State private var scanning = false
  #endif

  var body: some View {
    #if os(iOS)
      NavigationView {
        pairingContent
          .navigationTitle("Add Device")
          .navigationBarTitleDisplayMode(.inline)
          .toolbar {
            ToolbarItem(placement: .confirmationAction) {
              Button("Done", action: dismiss)
            }
          }
      }.navigationViewStyle(.stack)
    #else
      VStack(spacing: 0) {
        HStack {
          Text("Add Device").font(.title2.bold())
          Spacer()
          Button("Done", action: dismiss).keyboardShortcut(.cancelAction)
        }.padding(24)
        Divider()
        pairingContent
      }.frame(idealWidth: 520, idealHeight: 640)
    #endif
  }

  private var pairingContent: some View {
    ScrollView {
      VStack(alignment: .leading, spacing: 20) {
        Text(
          "Open Nearby Sync on both devices. Only connect devices you trust with this app’s data.")
        if model.pairingSucceeded {
          Label("Device added", systemImage: "checkmark.circle.fill").font(.title2.bold()).foregroundColor(.green)
          Text("Review shared data in Your devices if asked before syncing existing records.")
        } else {
          GroupBox("Show on this device") {
            VStack(alignment: .leading, spacing: 12) {
              Text(
                "On your other device, choose \(model.localDisplayName) and enter the code shown here."
              )
              VStack(alignment: .leading, spacing: 10) {
                Button("Show six-digit code") { model.offerCode() }
                  .buttonStyle(.borderedProminent)
                Button("Show QR instead") { model.offerQR() }
              }.disabled(model.isBusy)
              if let invitation = model.invitation { invitationView(invitation) }
            }.padding(6)
          }
          GroupBox("Connect to a nearby device") {
            TimelineView(.periodic(from: .now, by: 1)) { tick in
              VStack(alignment: .leading, spacing: 12) {
                Text("On the other device, tap Add Device, then Show six-digit code.")
                if availableDevices.isEmpty {
                  Text("Looking for devices… Keep both apps open on the same local network.")
                    .foregroundColor(.secondary)
                }
                ForEach(availableDevices, id: \.identity.device_id) { peer in
                  HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                      Text(peer.advertisement?.display_name ?? "Nearby device").font(.headline).fixedSize(horizontal: false, vertical: true)
                      Text(availability(peer, at: tick.date)).font(.caption).foregroundColor(
                        .secondary)
                    }
                    Spacer()
                    Button(selectedID == peer.identity.device_id ? "Selected" : "Choose") {
                      selectedID = peer.identity.device_id
                      code = ""
                    }.disabled(!model.canPair(peer, at: tick.date) || model.isBusy)
                  }
                }
                if let selected = selectedPeer {
                  TextField(
                    "Six-digit code for \(selected.advertisement?.display_name ?? "device")",
                    text: $code
                  )
                  .textFieldStyle(.roundedBorder)
                  .accessibilityLabel("Six-digit pairing code")
                  .onSubmit { connectSelected() }
                  .disabled(model.isBusy)
                  Button("Connect") { connectSelected() }
                    .buttonStyle(.borderedProminent)
                    .disabled(
                      !LibreSyncSessionModel.validCode(code)
                        || !model.canPair(selected, at: tick.date) || model.isBusy)
                  Text(
                    "Connecting grants this device access to this app’s sync group. Names alone don’t establish trust."
                  ).font(.caption).foregroundColor(.secondary)
                }
              }.padding(6)
            }
          }
          DisclosureGroup("Use a QR invitation") {
            VStack(alignment: .leading, spacing: 10) {
              #if os(iOS)
                Button("Scan QR code") { scanning = true }
                  .sheet(isPresented: $scanning) {
                    LibreSyncQRScanner { value in
                      scanning = false
                      encoded = value
                      confirmInvitation = true
                    }
                  }
              #endif
              TextField("Paste invitation", text: $encoded).textFieldStyle(.roundedBorder)
              Button("Review invitation") { confirmInvitation = true }.disabled(
                encoded.isEmpty || model.isBusy)
            }.padding(.top, 8)
          }
        }
        if model.isBusy { ProgressView("Working…") }
        if let error = model.errorMessage {
          Text(error).foregroundColor(.red).textSelection(.enabled)
          DisclosureGroup("Error details") {
            Text(model.error ?? "").font(.caption).textSelection(.enabled)
          }
          Text(
            "Check the code and try again. If it expired, ask the other device to show a new one."
          ).font(.caption)
        }
      }.padding(24)
    }
    .alert("Connect this device?", isPresented: $confirmInvitation) {
      Button("Cancel", role: .cancel) {}
      Button("Connect") { model.connect(encoded) }
    } message: {
      Text(
        "This invitation will be authenticated before granting access to this app’s data. Only continue if you received it from a device you trust."
      )
    }
  }

  private var availableDevices: [LibreSyncNearbyDevice] {
    let trusted = Set((model.snapshot?.peers ?? []).map { $0.identity.device_id })
    return model.nearby.filter { !trusted.contains($0.identity.device_id) }
  }
  private var selectedPeer: LibreSyncNearbyDevice? {
    availableDevices.first { $0.identity.device_id == selectedID }
  }
  private func connectSelected() {
    guard let peer = selectedPeer else { return }
    model.connect(peer, code: code)
  }
  private func availability(_ peer: LibreSyncNearbyDevice, at date: Date) -> String {
    if !model.compatible(peer) { return "Update both apps to compatible versions." }
    guard let invitation = peer.invitation else {
      return "Open Add Device on this device to show a code."
    }
    guard invitation.version == 2 else { return "Update both apps to use nearby pairing." }
    if Double(invitation.expires_at) <= date.timeIntervalSince1970 {
      return "Code expired. Show a new code on this device."
    }
    return "Ready to connect"
  }
  @ViewBuilder private func invitationView(_ invitation: LibreSyncInvitation) -> some View {
    TimelineView(.periodic(from: .now, by: 1)) { tick in
      let remaining = max(
        0, Double(invitation.invitation.expires_at) - tick.date.timeIntervalSince1970)
      if remaining == 0 {
        Text("Invitation expired.")
        Button("Show a new invitation") {
          if invitation.invitation.secret.count == 6 { model.offerCode() } else { model.offerQR() }
        }.disabled(model.isBusy)
      } else {
        VStack(alignment: .leading, spacing: 8) {
          if invitation.invitation.secret.count == 6 {
            Text(invitation.invitation.secret).font(
              .system(.largeTitle, design: .monospaced).weight(.semibold)
            )
            .textSelection(.enabled).accessibilityLabel(
              "Pairing code \(invitation.invitation.secret.map(String.init).joined(separator: " "))"
            )
          } else if let encoded = try? invitation.encoded(), let image = LibreSyncQR.image(encoded)
          {
            Image(decorative: image, scale: 1).interpolation(.none).resizable().scaledToFit()
              .frame(maxWidth: 230).accessibilityLabel("Private device pairing QR code")
          } else {
            Text("QR unavailable. Show a six-digit code instead.")
          }
          Label("Expires in \(Int(remaining)) seconds", systemImage: "clock")
            .font(.subheadline.monospacedDigit()).foregroundColor(.secondary)
          Text("Keep this invitation private.").font(.caption).foregroundColor(.secondary)
        }
      }
    }
  }
}
