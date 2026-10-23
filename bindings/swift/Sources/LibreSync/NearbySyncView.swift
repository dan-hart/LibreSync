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
    VStack(alignment: .leading, spacing: 20) {
      HStack(alignment: .top) {
        Image(systemName: "personalhotspot").font(.largeTitle).accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 5) {
          Text("Nearby Sync").font(.title2.bold())
          Text("Keep this app’s data together across your devices.").foregroundColor(.secondary)
        }
        Spacer()
      }
      GroupBox {
        HStack {
          VStack(alignment: .leading, spacing: 4) {
            Text(model.localDisplayName).font(.headline)
            Text(model.phaseDescription).font(.subheadline).foregroundColor(.secondary)
          }
          Spacer()
          if let onRename { Button("Rename", action: onRename).disabled(model.isBusy) }
        }.padding(6)
      }
      HStack {
        Text("Your devices").font(.headline)
        Spacer()
        Button {
          model.beginPairing()
          addingDevice = model.isPairing
        } label: {
          Label("Add Device", systemImage: "plus")
        }.disabled(model.isBusy)
      }
      if model.snapshot?.peers.isEmpty ?? true {
        Text("Add your first device to sync directly over your local network.")
          .foregroundColor(.secondary)
      } else {
        LibreSyncDevicesView(model: model).frame(minHeight: 140)
      }
      if let error = model.errorMessage {
        VStack(alignment: .leading, spacing: 6) {
          Label("Sync needs attention", systemImage: "exclamationmark.triangle").font(.headline)
          Text(error).textSelection(.enabled)
          Button("Retry Nearby Sync") { model.wake() }.disabled(model.isBusy)
        }.accessibilityElement(children: .combine)
      }
      DisclosureGroup("Connection help and details") {
        VStack(alignment: .leading, spacing: 10) {
          Text(
            "Keep both devices on the same local network and open this app on each device. Device names help you choose; the private code or invitation authenticates the connection."
          )
          Text(permissionHelp)
          if let technicalError = model.error {
            Text(technicalError).font(.caption).textSelection(.enabled)
          }
          Button("Check Local Network access") { model.requestPermission() }
          ForEach(Array((model.snapshot?.diagnostics ?? []).enumerated()), id: \.offset) {
            _, event in
            if case .diagnostic(let diagnostic) = event {
              Text(diagnostic.message).font(.caption).textSelection(.enabled)
            }
          }
        }.padding(.top, 8)
      }
    }
    .padding(20)
    .sheet(isPresented: $addingDevice, onDismiss: { model.endPairing() }) {
      NearbySyncAddDeviceView(model: model) { addingDevice = false }
    }
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
    ScrollView {
      VStack(alignment: .leading, spacing: 18) {
        HStack {
          Text("Add a device").font(.title2.bold())
          Spacer()
          Button("Done", action: dismiss).keyboardShortcut(.cancelAction)
        }
        Text(
          "Open Nearby Sync on both devices. Only connect devices you trust with this app’s data.")
        if model.pairingSucceeded {
          Label("Device added", systemImage: "checkmark.circle.fill").font(.headline)
          Text("Review shared data in Your devices if asked before syncing existing records.")
        } else {
          GroupBox("Show on this device") {
            VStack(alignment: .leading, spacing: 12) {
              Text(
                "On your other device, choose \(model.localDisplayName) and enter the code shown here."
              )
              HStack {
                Button("Show six-digit code") { model.offerCode() }
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
                  HStack {
                    VStack(alignment: .leading, spacing: 4) {
                      Text(peer.advertisement?.display_name ?? "Nearby device").font(.headline)
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
                  .onSubmit { connectSelected() }
                  .disabled(model.isBusy)
                  Button("Connect") { connectSelected() }
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
    .frame(idealWidth: 520, idealHeight: 610)
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
              .system(size: 34, weight: .semibold, design: .monospaced)
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
          Text("Expires in \(Int(remaining)) seconds. Keep this invitation private.").font(.caption)
        }
      }
    }
  }
}
