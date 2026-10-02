import SwiftUI
import CoreImage
import CoreImage.CIFilterBuiltins

@MainActor
public final class LibreSyncSessionModel:ObservableObject {
    public let session:LibreSyncSession
    @Published public private(set) var snapshot:LibreSyncSessionSnapshot?
    @Published public private(set) var nearby:[LibreSyncNearbyDevice]=[]
    @Published public private(set) var invitation:LibreSyncInvitation?
    @Published public private(set) var bootstrap:LibreSyncBootstrapPreview?
    @Published public private(set) var error:String?
    @Published public private(set) var permission:LibreSyncPermissionState = .unknown
    @Published public private(set) var compatibleContract:String?
    private var observer:Task<Void,Never>?
    private var lifecycle:Task<Void,Never>?
    private let bonjour:LibreSyncBonjour
    private let permissionRequest=LibreSyncLocalNetworkPermission()
    public init(session:LibreSyncSession){self.session=session;bonjour=LibreSyncBonjour(session:session)}
    public func start(){sequence {[self] in do{_ = try await self.session.resume();try await self.bonjour.start();try await self.refresh();self.observer?.cancel();self.observer=Task{[weak self,session=self.session] in do{for try await _ in session.events(){guard let owner=self else{break};try await owner.refresh()}}catch{if !Task.isCancelled{self?.error=error.localizedDescription}}}}catch{self.error=error.localizedDescription}}}
    public func stop(){observer?.cancel();observer=nil;sequence{await self.bonjour.stop();do{try await self.session.pause()}catch{self.error=error.localizedDescription}}}
    public func refresh()async throws {snapshot=try await session.snapshot();nearby=try await session.nearby();compatibleContract=try await session.platformAdvertisement().txt["contract"]}
    public func compatible(_ peer:LibreSyncNearbyDevice)->Bool {peer.advertisement?.contract_digest==compatibleContract && peer.invitation?.version==2}
    public func offerCode(){run{self.invitation=try await self.session.invitation(code:true)}}
    public func offerQR(){run{self.invitation=try await self.session.invitation()}}
    public func connect(_ encoded:String){run{_ = try await self.session.connect(encoded);try await self.refresh()}}
    public func connect(_ peer:LibreSyncNearbyDevice,code:String){run{_ = try await self.session.connect(peer,code:code);try await self.refresh()}}
    public func review(_ peer:LibreSyncPeer){run{self.bootstrap=try await self.session.bootstrap(peer:peer.identity.device_id)}}
    public func resolve(_ decision:LibreSyncBootstrapDecision){guard let preview=bootstrap else{return};run{try await self.session.resolve(preview,decision:decision);self.bootstrap=nil;try await self.refresh()}}
    public func resume(_ peer:LibreSyncPeer){run{try await self.session.resume(peer:peer.identity.device_id);try await self.refresh()}}
    public func pause(_ peer:LibreSyncPeer){run{try await self.session.pause(peer:peer.identity.device_id);try await self.refresh()}}
    public func remove(_ peer:LibreSyncPeer){run{try await self.session.remove(peer:peer.identity.device_id);try await self.refresh()}}
    public func repair(_ peer:LibreSyncPeer,invitation:String){run{_ = try await self.session.repair(peer:peer.identity.device_id,invitation:invitation);try await self.refresh()}}
    public func wake(){start()}
    public func requestPermission(){permissionRequest.requestState{[weak self] state in self?.setPermission(state)}}
    public func setPermission(_ state:LibreSyncPermissionState){permission=state;run{try await self.session.reportPermission(state)}}
    private func sequence(_ action:@escaping @MainActor ()async->Void){let previous=lifecycle;lifecycle=Task{await previous?.value;await action()}}
    private func run(_ action:@escaping @MainActor ()async throws->Void){Task{do{try await action();error=nil}catch{self.error=error.localizedDescription}}}
    deinit{observer?.cancel()}
}
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
                    VStack(alignment:.leading){Text(peer.advertisement?.display_name ?? "Nearby device");if model.compatible(peer){Button("Enter this device’s code"){selected=peer;confirmCode=true}}else{Text(peer.invitation?.version != 2 ? "Update both devices or open pairing on that device." : "This app’s schema differs. Update both apps.").font(.caption)}}
                }
            }
            if let error=model.error {Section("Needs attention"){Text(error)}}
        }
        .alert("Connect this app’s sync group?",isPresented:$confirmQR){Button("Cancel",role:.cancel){};Button("Connect"){model.connect(encoded)}}message:{Text(invitationName(encoded))}
        .sheet(isPresented:$confirmCode){VStack(spacing:16){Text(selected?.advertisement?.display_name ?? "Nearby device").font(.headline);Text("Enter the six-digit code shown on this device to grant access to this app’s sync group.");TextField("Six-digit code",text:$code);Button("Connect"){if let selected{model.connect(selected,code:code)};confirmCode=false};Button("Cancel"){confirmCode=false}}.padding()}
    }
    private func invitationName(_ value:String)->String {guard value.utf8.count<=16*1024,let data=value.data(using:.utf8),let root=try? JSONSerialization.jsonObject(with:data) as? [String:Any],let invitation=root["invitation"] as? [String:Any],let metadata=invitation["metadata"] as? [String:Any],let name=metadata["display_name"] as? String else{return "The invitation will be validated and its certificate pinned before trust is granted."};return "Connect "+String(name.prefix(128))+"? The invitation and certificate will be authenticated before enrollment."}
}
public struct LibreSyncDevicesView:View {
    @ObservedObject private var model:LibreSyncSessionModel
    @State private var removing:LibreSyncPeer?
    @State private var repairing:LibreSyncPeer?
    @State private var replacement=""
    public init(model:LibreSyncSessionModel){self.model=model}
    public var body:some View {
        List {ForEach(model.snapshot?.peers ?? [],id:\.identity.device_id){peer in
            VStack(alignment:.leading){Text(peer.metadata.display_name).font(.headline);Text(peer.revoked ? "Removed — stored data retained" : String(describing:peer.state));Text("Pending records: \(peer.pending)").font(.caption)
                HStack{if peer.state == .needsMerge{Button("Review combine"){model.review(peer)}};if !peer.revoked{if peer.state == .paused{Button("Resume"){model.resume(peer)}}else{Button("Pause"){model.pause(peer)}}};Button("Repair"){repairing=peer};Button("Remove",role:.destructive){removing=peer}}
            }
        }}
        .alert("Remove device trust?",isPresented:Binding(get:{removing != nil},set:{if !$0{removing=nil}})){Button("Cancel",role:.cancel){removing=nil};Button("Remove",role:.destructive){if let peer=removing{model.remove(peer)};removing=nil}}message:{Text("Existing local data stays on this device. Reconnecting requires a fresh invitation and explicit repair.")}
        .sheet(isPresented:Binding(get:{repairing != nil},set:{if !$0{repairing=nil}})){VStack{Text("Repair with a fresh invitation from this device");TextField("Replacement invitation",text:$replacement);Button("Repair trust"){if let peer=repairing{model.repair(peer,invitation:replacement)};repairing=nil};Button("Cancel"){repairing=nil}}.padding()}
        .sheet(isPresented:Binding(get:{model.bootstrap != nil},set:{if !$0{model.resolve(.cancel)}})){VStack(spacing:16){Text("Combine existing data?").font(.headline);Text("A recovery copy is kept before combining. Local and incoming records use the selected app’s reviewed merge policy.");Text("Incoming records: \(model.bootstrap?.batch.records.count ?? 0)");Button("Combine"){model.resolve(.combine)};Button("Cancel"){model.resolve(.cancel)}}.padding()}
    }
}
public struct LibreSyncStatusView:View {
    @ObservedObject private var model:LibreSyncSessionModel
    public init(model:LibreSyncSessionModel){self.model=model}
    public var body:some View {VStack(alignment:.leading,spacing:10){Text("Sync: \(model.snapshot?.phase.rawValue ?? "Stopped")");Text("Local Network: \(model.permission.rawValue)");Button("Retry / reconnect"){model.wake()};if let error=model.error{Text(error)};ForEach(Array((model.snapshot?.diagnostics ?? []).enumerated()),id:\.offset){_,event in if case .diagnostic(let d)=event{Text(d.message);Text(guidance(d.action)).font(.caption)}}}}
    private func guidance(_ action:LibreSyncDiagnosticAction)->String {switch action{case .grantPermission,.checkPermissions:return "Allow Local Network in system Settings, check Wi-Fi, then retry.";case .repairPeer:return "Get a fresh invitation from the device and repair trust in Devices.";case .reviewMerge:return "Review Combine or Cancel in Devices.";case .checkSecureStorage:return "Unlock secure storage and retry. Do not regenerate the identity.";case .reviewRecords:return "Check record size and app schema; retained records remain available.";case .reviewCompatibility:return "Update both devices to compatible app and pairing versions.";default:return action.rawValue}}
}
