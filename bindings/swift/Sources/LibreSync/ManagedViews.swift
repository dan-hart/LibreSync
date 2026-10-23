import SwiftUI
import CoreImage
import CoreImage.CIFilterBuiltins

public enum LibreSyncQR {
    public static func image(_ invitation:String)->CGImage? {
        guard invitation.utf8.count<=16*1024 else{return nil}
        let filter=CIFilter.qrCodeGenerator();filter.message=Data(invitation.utf8);filter.correctionLevel="M"
        guard let output=filter.outputImage?.transformed(by:CGAffineTransform(scaleX:6,y:6)) else{return nil}
        return CIContext().createCGImage(output,from:output.extent)
    }
    public static func decode(_ image:CGImage)->String? {
        let detector=CIDetector(ofType:CIDetectorTypeQRCode,context:CIContext(),options:[CIDetectorAccuracy:CIDetectorAccuracyHigh])
        guard let value=(detector?.features(in:CIImage(cgImage:image)).first as? CIQRCodeFeature)?.messageString,value.utf8.count<=16*1024 else{return nil};return value
    }
}
public struct LibreSyncConnectView:View {
    @ObservedObject private var model:LibreSyncSessionModel
    @State private var encoded=""
    @State private var code=""
    @State private var selected:LibreSyncNearbyDevice?
    @State private var confirmQR=false
    @State private var confirmCode=false
    #if os(iOS)
    @State private var scanning=false
    #endif
    public init(model:LibreSyncSessionModel){self.model=model}
    public var body:some View {
        Form {
            Section("Connect a device") {Button("Check Local Network access"){model.requestPermission()};Text("If access is denied, enable Local Network in system Settings, then retry. A timeout remains Unknown.");Text("Devices sync directly on your local network. Connecting grants the selected device access to this app’s sync group.")
                HStack{Button("Show QR"){model.offerQR()};Button("Show six-digit code"){model.offerCode()}}
                if let invitation=model.invitation {
                    TimelineView(.periodic(from:.now,by:1)){tick in
                    let remaining=max(0,Double(invitation.invitation.expires_at)-tick.date.timeIntervalSince1970)
                    if remaining==0 {Text("Invitation expired. Renew it before connecting.");Button("Renew invitation"){if invitation.invitation.secret.count==6{model.offerCode()}else{model.offerQR()}}}else{
                    if invitation.invitation.secret.count==6 {Text(invitation.invitation.secret).font(.largeTitle.monospacedDigit()).textSelection(.enabled)}
                    else if let encoded=try? invitation.encoded(),let image=LibreSyncQR.image(encoded){Image(decorative:image,scale:1).interpolation(.none).resizable().scaledToFit().frame(maxWidth:280)}else{Text("Invitation is too large for QR. Use a nearby six-digit code.")}
                    Text("Expires in \(Int(remaining)) seconds. Keep the QR and code private.")
                    }
                    }
                }
                #if os(iOS)
                Button("Scan QR"){scanning=true}.sheet(isPresented:$scanning){LibreSyncQRScanner{value in scanning=false;encoded=value;confirmQR=true}}
                #endif
                TextField("Paste invitation as an alternative",text:$encoded)
                Button("Connect invitation"){confirmQR=true}.disabled(encoded.isEmpty)
            }
            Section("Nearby devices") {
                if model.nearby.isEmpty{Text("No compatible device found yet. Check Wi-Fi and Local Network access. An empty list does not establish permission denial.")}
                ForEach(model.nearby,id:\.identity.device_id){peer in
                    VStack(alignment:.leading){Text(peer.advertisement?.display_name ?? "Nearby device");if model.canPair(peer){Button("Enter this device’s code"){selected=peer;confirmCode=true}}else{Text(peer.invitation?.version != 2 ? "Update both devices or open pairing on that device." : "This app’s schema differs. Update both apps.").font(.caption)}}
                }
            }
            if let error=model.error {Section("Needs attention"){Text(error)}}
        }
        .onDisappear { model.endPairing() }
        .alert("Connect this app’s sync group?",isPresented:$confirmQR){Button("Cancel",role:.cancel){};Button("Connect"){model.connect(encoded)}}message:{Text(invitationName(encoded))}
        .sheet(isPresented: $confirmCode) {
            LibreSyncConfirmationSheet(
                title: "Connect device", actionTitle: "Connect",
                cancel: { confirmCode = false },
                confirm: {
                    if let selected { model.connect(selected, code: code) }
                    confirmCode = false
                }
            ) {
                Text(selected?.advertisement?.display_name ?? "Nearby device").font(.headline)
                Text("Enter the six-digit code shown on this device to grant access to this app’s sync group.")
                TextField("Six-digit code", text: $code).textFieldStyle(.roundedBorder)
            }
        }
    }
    private func invitationName(_ value:String)->String {guard value.utf8.count<=16*1024,let data=value.data(using:.utf8),let root=try? JSONSerialization.jsonObject(with:data) as? [String:Any],let invitation=root["invitation"] as? [String:Any],let metadata=invitation["metadata"] as? [String:Any],let name=metadata["display_name"] as? String else{return "The invitation will be validated and its certificate pinned before trust is granted."};return "Connect "+String(name.prefix(128))+"? The invitation and certificate will be authenticated before enrollment."}
}
/// Non-scrolling device rows that fit in a host Form, List section, or scroll view.
public struct LibreSyncDevicesView: View {
    @ObservedObject private var model: LibreSyncSessionModel
    @State private var removing: LibreSyncPeer?
    @State private var repairing: LibreSyncPeer?
    @State private var replacement = ""

    public init(model: LibreSyncSessionModel) { self.model = model }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(model.snapshot?.peers ?? [], id: \.identity.device_id) { peer in
                deviceRow(peer)
                if peer.identity.device_id != model.snapshot?.peers.last?.identity.device_id {
                    Divider().padding(.vertical, 12)
                }
            }
        }
        .alert("Remove device trust?", isPresented: Binding(
            get: { removing != nil }, set: { if !$0 { removing = nil } }
        )) {
            Button("Cancel", role: .cancel) { removing = nil }
            Button("Remove", role: .destructive) {
                if let peer = removing { model.remove(peer) }
                removing = nil
            }
        } message: {
            Text("Existing local data stays on this device. Reconnecting requires a fresh invitation and explicit repair.")
        }
        .sheet(isPresented: Binding(
            get: { repairing != nil }, set: { if !$0 { repairing = nil } }
        )) {
            LibreSyncConfirmationSheet(
                title: "Reconnect device", actionTitle: "Repair trust",
                actionDisabled: replacement.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isBusy,
                cancel: { repairing = nil },
                confirm: {
                    if let peer = repairing { model.repair(peer, invitation: replacement) }
                    repairing = nil
                }
            ) {
                Text(repairing?.metadata.display_name ?? "Your device").font(.headline)
                Text("Use a fresh invitation from this device to repair its trusted connection.")
                    .foregroundColor(.secondary)
                TextField("Replacement invitation", text: $replacement).textFieldStyle(.roundedBorder)
            }
        }
        .sheet(isPresented: Binding(
            get: { model.bootstrap != nil }, set: { if !$0 { model.resolve(.cancel) } }
        )) {
            LibreSyncConfirmationSheet(
                title: "Combine existing data?", actionTitle: "Combine",
                cancel: { model.resolve(.cancel) },
                confirm: { model.resolve(.combine) }
            ) {
                Text("A recovery copy is kept before combining. Local and incoming records use the selected app’s reviewed merge policy.")
                    .foregroundColor(.secondary)
                Text("Incoming records: \(model.bootstrap?.batch.records.count ?? 0)").font(.headline)
            }
        }
    }

    private func deviceRow(_ peer: LibreSyncPeer) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: peer.revoked ? "link.badge.plus" : "laptopcomputer.and.iphone")
                    .font(.title3).foregroundColor(.secondary).frame(width: 28)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(peer.metadata.display_name).font(.headline)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(peer.revoked ? "Removed — stored data retained" : LibreSyncSessionModel.status(for: peer.state))
                        .font(.subheadline).foregroundColor(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if peer.state == .needsMerge {
                Button("Review shared data") { model.review(peer) }
                    .buttonStyle(.bordered).disabled(model.isBusy)
            }
            HStack(spacing: 16) {
                if !peer.revoked {
                    if peer.state == .paused {
                        Button("Resume") { model.resume(peer) }
                    } else {
                        Button("Pause") { model.pause(peer) }
                    }
                }
                Spacer(minLength: 0)
                Menu {
                    Button("Reconnect with invitation") { replacement = ""; repairing = peer }
                    Button("Remove device", role: .destructive) { removing = peer }
                } label: {
                    Label("Manage", systemImage: "ellipsis.circle")
                }.accessibilityLabel("Manage \(peer.metadata.display_name)")
            }.buttonStyle(.borderless).disabled(model.isBusy)
            DisclosureGroup("Device details") {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Changes awaiting confirmation: \(peer.pending)")
                    Text("Verified device fingerprint: \(peer.fingerprint)").textSelection(.enabled)
                }.font(.caption).foregroundColor(.secondary).padding(.vertical, 6)
            }.font(.subheadline)
        }.padding(.vertical, 6)
    }
}
public struct LibreSyncStatusView:View {
    @ObservedObject private var model:LibreSyncSessionModel
    public init(model:LibreSyncSessionModel){self.model=model}
    public var body:some View {VStack(alignment:.leading,spacing:10){Text(model.phaseDescription);Text("Local Network: \(model.permission.rawValue)");Button("Retry / reconnect"){model.wake()};if let error=model.error{Text(error)};ForEach(Array((model.snapshot?.diagnostics ?? []).enumerated()),id:\.offset){_,event in if case .diagnostic(let d)=event{Text(d.message);Text(guidance(d.action)).font(.caption)}}}}
    private func guidance(_ action:LibreSyncDiagnosticAction)->String {switch action{case .grantPermission,.checkPermissions:return "Allow Local Network in system Settings, check Wi-Fi, then retry.";case .repairPeer:return "Get a fresh invitation from the device and repair trust in Devices.";case .reviewMerge:return "Review shared data in Devices.";case .checkSecureStorage:return "Unlock secure storage and retry. Do not regenerate the identity.";case .reviewRecords:return "Check record size and app schema; retained records remain available.";case .reviewCompatibility:return "Update both devices to compatible app and pairing versions.";default:return action.rawValue}}
}

/// Keeps consent actions reachable when large text or the keyboard reduces the sheet's content area.
private struct LibreSyncConfirmationSheet<Content: View>: View {
    let title: String
    let actionTitle: String
    var actionDisabled = false
    let cancel: () -> Void
    let confirm: () -> Void
    @ViewBuilder var content: () -> Content

    var body: some View {
        #if os(iOS)
        NavigationView {
            scrollableContent
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel", action: cancel)
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button(actionTitle, action: confirm).disabled(actionDisabled)
                    }
                }
        }.navigationViewStyle(.stack)
        #else
        VStack(spacing: 0) {
            Text(title).font(.title2.bold())
                .frame(maxWidth: .infinity, alignment: .leading).padding(24)
            Divider()
            scrollableContent
            Divider()
            HStack {
                Button("Cancel", action: cancel).keyboardShortcut(.cancelAction)
                Spacer()
                Button(actionTitle, action: confirm).buttonStyle(.borderedProminent)
                    .disabled(actionDisabled)
            }.padding(20)
        }.frame(minWidth: 320, idealWidth: 440, idealHeight: 380)
        #endif
    }

    private var scrollableContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                content()
            }
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(24)
        }
    }
}
