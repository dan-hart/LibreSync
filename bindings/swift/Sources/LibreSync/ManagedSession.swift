import Foundation
import CLibreSync

public protocol LibreSyncSecureStore: Sendable {
    func get(_ name: String) throws -> Data?
    func set(_ name: String, value: Data) throws
    func delete(_ name: String) throws
}
public struct LibreSyncKeychainStore: LibreSyncSecureStore {
    public let service: String
    public init(service: String) { self.service = service }
    public func get(_ name: String) throws -> Data? { try LibreSyncKeychain.load(account: name, service: service) }
    public func set(_ name: String, value: Data) throws { try LibreSyncKeychain.save(data: value, account: name, service: service) }
    public func delete(_ name: String) throws { try LibreSyncKeychain.delete(account: name, service: service) }
}
public enum LibreSyncSessionErrorCode: String, Codable, Sendable {
    case closed = "Closed", cancelled = "Cancelled", invalidOperation = "InvalidOperation"
    case invalidConfiguration = "InvalidConfiguration", invalidAdapter = "InvalidAdapter", invalidRecord = "InvalidRecord"
    case incompatibleSchema = "IncompatibleSchema", staleBootstrap = "StaleBootstrap", groupConflict = "GroupConflict", peerRevoked = "PeerRevoked"
    case storageUnavailable = "StorageUnavailable", storageCommitUncertain = "StorageCommitUncertain", busy = "Busy", operationFailed = "OperationFailed"
}
public struct LibreSyncSessionError: Error, Codable, Sendable, LocalizedError {
    public let code: LibreSyncSessionErrorCode
    public let message: String
    public var errorDescription: String? { message }
}
public enum LibreSyncPermissionState: String, Codable, Sendable { case allowed = "Allowed", denied = "Denied", unknown = "Unknown" }
public enum LibreSyncJSON: Codable, Sendable, Equatable {
    case null, bool(Bool), number(Decimal), string(String), array([LibreSyncJSON]), object([String: LibreSyncJSON])
    public init(from decoder: Decoder) throws {
        let c=try decoder.singleValueContainer()
        if c.decodeNil(){self = .null} else if let v=try? c.decode(Bool.self){self = .bool(v)} else if let v=try? c.decode(String.self){self = .string(v)} else if let v=try? c.decode(Decimal.self){self = .number(v)} else if let v=try? c.decode([LibreSyncJSON].self){self = .array(v)} else { self = .object(try c.decode([String:LibreSyncJSON].self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var c=encoder.singleValueContainer()
        switch self {case .null:try c.encodeNil();case .bool(let v):try c.encode(v);case .number(let v):try c.encode(v);case .string(let v):try c.encode(v);case .array(let v):try c.encode(v);case .object(let v):try c.encode(v)}
    }
}
public struct LibreSyncAdapterDescriptor: Codable, Sendable, Equatable {
    public let id: String, namespace: String, schema: String
    public let transactional: Bool
    public init(id:String,namespace:String,schema:String="logical-records-v1",transactional:Bool=true){self.id=id;self.namespace=namespace;self.schema=schema;self.transactional=transactional}
}
public struct LibreSyncManifest: Codable, Sendable, Equatable {
    public let app_id: String, display_name: String
    public let schema_version: UInt32
    public let adapters: [LibreSyncAdapterDescriptor]
    public init(appID:String,displayName:String,schemaVersion:UInt32=1,adapters:[LibreSyncAdapterDescriptor]){app_id=appID;display_name=displayName;schema_version=schemaVersion;self.adapters=adapters}
}
public struct LibreSyncSessionConfiguration: Codable, Sendable {
    public enum Policy:String,Codable,Sendable {case notes,momentum,records}
    public let policy: Policy
    public let manifest: LibreSyncManifest?
    public let state_dir: String, display_name: String, device_kind: String, role: String
    public let listen: String?
    public let advertise: Bool
    public let authenticated_io_timeout_ms: UInt64
    public init(policy:Policy,manifest:LibreSyncManifest?=nil,stateDirectory:URL,displayName:String,deviceKind:String="desktop",role:String="device",listen:String?=nil,authenticatedIOTimeout:TimeInterval=60) throws {
        guard authenticatedIOTimeout>0,authenticatedIOTimeout<=300 else{throw LibreSyncSessionError(code:.invalidConfiguration,message:"Authenticated IO timeout must be greater than zero and at most five minutes")}
        self.policy=policy;self.manifest=manifest;state_dir=stateDirectory.path;display_name=displayName;device_kind=deviceKind;self.role=role;self.listen=listen;advertise=false;authenticated_io_timeout_ms=UInt64(authenticatedIOTimeout*1000)
    }
    public static func notes(stateDirectory:URL,displayName:String)throws->Self{try .init(policy:.notes,stateDirectory:stateDirectory,displayName:displayName)}
}
public struct LibreSyncIdentity: Codable,Sendable,Equatable {public let device_id:String,user_id:String,app_id:String}
public struct LibreSyncMetadata: Codable,Sendable {public let display_name:String,device_kind:String,role:String;public let manifest:LibreSyncManifest}
public struct LibreSyncClock: Codable,Sendable,Equatable {public let counter:UInt64;public let device_id:String;public init(counter:UInt64,deviceID:String){self.counter=counter;device_id=deviceID}}
public struct LibreSyncRecord: Codable,Sendable,Equatable {public let adapter:String,id:String;public let value:Data;public let deleted:Bool;public let clock:LibreSyncClock;public init(adapter:String,id:String,value:Data,deleted:Bool=false,clock:LibreSyncClock){self.adapter=adapter;self.id=id;self.value=value;self.deleted=deleted;self.clock=clock}}
public struct LibreSyncReceipt: Codable,Sendable,Equatable {
    public let epoch:String;public let sequence:UInt64;public let proof:Data
    public init(epoch:String,sequence:UInt64,proof:Data){self.epoch=epoch;self.sequence=sequence;self.proof=proof}
}
public struct LibreSyncInbox: Codable,Sendable {public let revision:UInt64;public let records:[LibreSyncRecord];public let receipts:[String:LibreSyncReceipt]}
public enum LibreSyncPhase:String,Codable,Sendable {case stopped="Stopped",failed="Failed",running="Running",paused="Paused"}
public enum LibreSyncPeerState:String,Codable,Sendable {case waiting="Waiting",exchanging="Exchanging",stored="Stored",upToDate="UpToDate",needsMerge="NeedsMerge",needsRepair="NeedsRepair",paused="Paused"}
public struct LibreSyncPeer:Codable,Sendable {
    public let identity:LibreSyncIdentity,metadata:LibreSyncMetadata
    public let fingerprint:String,revoked:Bool,pending:UInt64
    public let stored:LibreSyncReceipt,applied:LibreSyncReceipt,transmitted:LibreSyncReceipt,received:LibreSyncReceipt,processed:LibreSyncReceipt
    public let state:LibreSyncPeerState
}
public enum LibreSyncDiagnosticAction:String,Codable,Sendable {case none="None",checkPermissions="CheckPermissions",retry="Retry",repairPeer="RepairPeer",reviewMerge="ReviewMerge",grantPermission="GrantPermission",checkSecureStorage="CheckSecureStorage",reviewCompatibility="ReviewCompatibility",reviewRecords="ReviewRecords",resolveGroupConflict="ResolveGroupConflict",reviewConfiguration="ReviewConfiguration",wait="Wait"}
public struct LibreSyncDiagnostic:Codable,Sendable {public let peer:String?,message:String,action:LibreSyncDiagnosticAction,evidence:LibreSyncJSON}
public enum LibreSyncSessionEvent:Codable,Sendable {
    case changed,diagnostic(LibreSyncDiagnostic)
    public init(from d:Decoder)throws{let c=try d.singleValueContainer();if (try? c.decode(String.self))=="Changed"{self = .changed}else{guard let diagnostic=try c.decode([String:LibreSyncDiagnostic].self)["Diagnostic"] else{throw DecodingError.dataCorruptedError(in:c,debugDescription:"Unknown session event")};self = .diagnostic(diagnostic)} }
    public func encode(to e:Encoder)throws{var c=e.singleValueContainer();switch self{case .changed:try c.encode("Changed");case .diagnostic(let d):try c.encode(["Diagnostic":d])}}
}
public struct LibreSyncSessionSnapshot:Codable,Sendable {public let identity:LibreSyncIdentity,phase:LibreSyncPhase,local_revision:UInt64,peers:[LibreSyncPeer],diagnostics:[LibreSyncSessionEvent]}
public struct LibreSyncPairingDescriptor:Codable,Sendable {public let version:UInt32,invitation_id:String,inviter_fingerprint:String,expires_at:UInt64}
public struct LibreSyncPairingCredential:Codable,Sendable {public let version:UInt32,invitation_id:String,inviter_fingerprint:String,metadata:LibreSyncMetadata,expires_at:UInt64,secret:String}
public struct LibreSyncInvitation:Codable,Sendable {
    public let invitation:LibreSyncPairingCredential,identity:LibreSyncIdentity,addresses:[String]
    public func encoded()throws->String{String(decoding:try JSONEncoder().encode(self),as:UTF8.self)}
}
public struct LibreSyncEnrollment:Codable,Sendable {public let invitation_id:String,identity:LibreSyncIdentity,metadata:LibreSyncMetadata,fingerprint:String}
public struct LibreSyncAdvertisement:Codable,Sendable {public let display_name:String,device_kind:String,role:String,app_display_name:String,schema_version:UInt32,contract_digest:String}
public struct LibreSyncNearbyDevice:Codable,Sendable {public let identity:LibreSyncIdentity,addresses:[String],advertisement:LibreSyncAdvertisement?,invitation:LibreSyncPairingDescriptor?}
public struct LibreSyncExportBatch:Codable,Sendable {public let checkpoint:LibreSyncReceipt,records:[LibreSyncRecord],full:Bool}
public struct LibreSyncBootstrapPreview:Codable,Sendable {public let token:String,peer:String,fingerprint:String,local_revision:UInt64,batch:LibreSyncExportBatch}
public enum LibreSyncBootstrapDecision:String,Codable,Sendable {case combine="Merge",cancel="Cancel"}
public struct LibreSyncRecoverySnapshot:Codable,Sendable {public let id:String,revision:UInt64,records:[LibreSyncRecord]}
public struct LibreSyncPlatformAdvertisement:Codable,Sendable {public let service_type:String,instance:String,port:UInt16,ipv6:Bool,txt:[String:String]}
private struct Envelope<T:Decodable>:Decodable {
    let abi:UInt32,ok:Bool,value:T?,error:LibreSyncSessionError?
    enum CodingKeys:String,CodingKey{case abi,ok,value,error}
    init(from decoder:Decoder)throws{let c=try decoder.container(keyedBy:CodingKeys.self);abi=try c.decode(UInt32.self,forKey:.abi);ok=try c.decode(Bool.self,forKey:.ok);error=try c.decodeIfPresent(LibreSyncSessionError.self,forKey:.error);value=c.contains(.value) ? .some(try T(from:c.superDecoder(forKey:.value))) : nil}
}
private final class KeyContext: @unchecked Sendable {let store:any LibreSyncSecureStore;init(_ s:any LibreSyncSecureStore){store=s}}
private let keyCall: @convention(c) (UnsafeMutableRawPointer?,UInt32,UnsafePointer<CChar>?,UnsafePointer<UInt8>?,Int,UnsafeMutablePointer<UnsafePointer<UInt8>?>?,UnsafeMutablePointer<Int>?)->Int32 = {context,op,name,bytes,count,output,length in
    guard let context,let name else{return -1}
    let store=Unmanaged<KeyContext>.fromOpaque(context).takeUnretainedValue().store
    do {switch op {
    case 0:guard let data=try store.get(String(cString:name)) else{return 1};let buffer=UnsafeMutablePointer<UInt8>.allocate(capacity:max(data.count,1));data.copyBytes(to:buffer,count:data.count);output?.pointee=UnsafePointer(buffer);length?.pointee=data.count
    case 1:guard let bytes else{return -1};try store.set(String(cString:name),value:Data(bytes:bytes,count:count))
    case 2:try store.delete(String(cString:name))
    default:return -1
    };return 0}catch{return -1}
}
private let releaseBuffer:@convention(c)(UnsafeMutableRawPointer?,UnsafePointer<UInt8>?,Int)->Void = {_,bytes,_ in bytes.map{UnsafeMutablePointer(mutating:$0).deallocate()} }
private let releaseContext:@convention(c)(UnsafeMutableRawPointer?)->Void = {context in if let context{Unmanaged<KeyContext>.fromOpaque(context).release()} }

/// Session owns its journal. Save one coherent inbox durably in your app before
/// acknowledge; a successful network transfer alone never means Applied.
private final class NativeCallGate:@unchecked Sendable {
    private enum State {case queued,started,cancelled}
    private let lock=NSLock()
    private var state:State = .queued
    func begin()->Bool {lock.lock();defer{lock.unlock()};guard state == .queued else{return false};state = .started;return true}
    @discardableResult func cancel()->Bool {lock.lock();defer{lock.unlock()};let started=state == .started;state = .cancelled;return started}
}
private struct InboxAcknowledgement:Encodable {let revision:UInt64;let receipts:[String:LibreSyncReceipt]}
public final class LibreSyncSession: @unchecked Sendable {
    private let lock=NSLock()
    private var handle:UInt64
    private var closing:Task<Void,Never>?
    private init(_ handle:UInt64){self.handle=handle}
    private func current()->UInt64{lock.lock();defer{lock.unlock()};return handle}
    private func cleanup()->Task<Void,Never>{lock.lock();defer{lock.unlock()};if let closing{return closing};let h=handle;handle=0;let task=Task.detached {if let p=libresync_session_close(h){libresync_string_free(p)}};closing=task;return task}
    deinit {let h=handle;if h != 0 {Task.detached {if let p=libresync_session_close(h){libresync_string_free(p)}}}}
    private static func decode<T:Decodable>(_ p:UnsafeMutablePointer<CChar>?,as:T.Type)throws->T {
        guard let p else{throw LibreSyncSessionError(code:.operationFailed,message:"Native response unavailable")};defer{libresync_string_free(p)}
        let envelope=try JSONDecoder().decode(Envelope<T>.self,from:Data(String(cString:p).utf8))
        guard envelope.abi==1 else{throw LibreSyncSessionError(code:.incompatibleSchema,message:"Unsupported managed ABI")}
        if let error=envelope.error{throw error}
        guard envelope.ok,let value=envelope.value else{throw LibreSyncSessionError(code:.operationFailed,message:"Invalid native response")}
        return value
    }
    public static func open(_ config:LibreSyncSessionConfiguration,keys:any LibreSyncSecureStore)async throws->LibreSyncSession {
        try Task.checkCancellation()
        let gate=NativeCallGate()
        let encoded=String(decoding:try JSONEncoder().encode(config),as:UTF8.self)
        let session=try await withTaskCancellationHandler(operation:{try Task.checkCancellation();return try await Task.detached {
            guard gate.begin() else{throw CancellationError()}
            let ctx=Unmanaged.passRetained(KeyContext(keys)).toOpaque()
            let callbacks=libresync_key_callbacks(context:ctx,call:keyCall,release_buffer:releaseBuffer,release_context:releaseContext)
            let h:UInt64=try decode(encoded.withCString{libresync_session_open($0,callbacks)},as:UInt64.self)
            return LibreSyncSession(h)
        }.value},onCancel:{gate.cancel()})
        if Task.isCancelled{await session.close();throw CancellationError()}
        return session
    }
    public func close()async {await cleanup().value}
    public func cancel()async {let _:LibreSyncJSON?=try? await request("cancel")}
    public func request<T:Decodable & Sendable>(_ op:String,fields:[String:LibreSyncJSON]=[:],as:T.Type=T.self)async throws->T {
        try Task.checkCancellation()
        let gate=NativeCallGate()
        let h=current();guard h != 0 else{throw LibreSyncSessionError(code:.closed,message:"Session closed")}
        var body=fields;body["op"] = .string(op)
        let encoded=String(decoding:try JSONEncoder().encode(body),as:UTF8.self)
        return try await withTaskCancellationHandler(operation:{
            try Task.checkCancellation()
            let result:T=try await Task.detached {guard gate.begin() else{throw CancellationError()};return try Self.decode(encoded.withCString{libresync_session_call(h,$0)},as:T.self)}.value
            if op != "subscribe"{try Task.checkCancellation()};return result
        },onCancel:{let started=gate.cancel();guard started && ["connect","connect_code","repair","recover_pairing"].contains(op) else{return};Task.detached {if let p="{\"op\":\"cancel\"}".withCString({libresync_session_call(h,$0)}){libresync_string_free(p)}}})
    }
    private func unit(_ op:String,_ fields:[String:LibreSyncJSON]=[:])async throws {let _:LibreSyncJSON=try await request(op,fields:fields)}
    private func field<T:Encodable>(_ value:T)throws->LibreSyncJSON{try JSONDecoder().decode(LibreSyncJSON.self,from:JSONEncoder().encode(value))}
    public func start()async throws->String{try await request("start")}
    public func shutdown()async throws{try await unit("shutdown")}
    public func pause()async throws{try await unit("pause")}
    public func resume()async throws->String{try await request("resume")}
    public func wake()async throws{try await unit("wake")}
    public func snapshot()async throws->LibreSyncSessionSnapshot{try await request("snapshot")}
    public func set(adapter:String,id:String,value:Data)async throws{try await unit("set",["adapter":.string(adapter),"id":.string(id),"value":.string(value.base64EncodedString())])}
    public func delete(adapter:String,id:String)async throws{try await unit("delete",["adapter":.string(adapter),"id":.string(id)])}
    public func records(adapter:String)async throws->[LibreSyncRecord]{try await request("records",fields:["adapter":.string(adapter)])}
    public func inbox()async throws->LibreSyncInbox{try await request("inbox")}
    public func acknowledge(_ inbox:LibreSyncInbox)async throws{try await unit("acknowledge_inbox",["inbox":try field(InboxAcknowledgement(revision:inbox.revision,receipts:inbox.receipts))])}
    public func importRecords(_ records:[LibreSyncRecord])async throws{try await unit("import",["records":try field(records)])}
    public func invitation(code:Bool=false,ttl:TimeInterval=120)async throws->LibreSyncInvitation{guard ttl>=0.001,ttl<=300 else{throw LibreSyncSessionError(code:.invalidConfiguration,message:"Invitation expiry must be one millisecond to five minutes")};return try await request("invitation",fields:["code":.bool(code),"ttl_ms":.number(Decimal(ttl*1000))])}
    public func closePairing()async throws{try await unit("close_pairing")}
    public func connect(_ encoded:String)async throws->LibreSyncEnrollment{try await request("connect",fields:["invitation":.string(encoded)])}
    public func connect(_ peer:LibreSyncNearbyDevice,code:String)async throws->LibreSyncEnrollment{try await request("connect_code",fields:["peer":try field(peer),"code":.string(code)])}
    public func repair(peer:String,invitation:String)async throws->LibreSyncEnrollment{try await request("repair",fields:["peer":.string(peer),"invitation":.string(invitation)])}
    public func remove(peer:String)async throws{try await unit("remove_peer",["peer":.string(peer)])}
    public func pause(peer:String)async throws{try await unit("pause_peer",["peer":.string(peer)])}
    public func resume(peer:String)async throws{try await unit("resume_peer",["peer":.string(peer)])}
    public func nearby()async throws->[LibreSyncNearbyDevice]{try await request("discovery")}
    public func bootstrap(peer:String)async throws->LibreSyncBootstrapPreview?{try await request("bootstrap_preview",fields:["peer":.string(peer)])}
    public func resolve(_ preview:LibreSyncBootstrapPreview,decision:LibreSyncBootstrapDecision)async throws{try await unit("resolve_bootstrap",["preview":try field(preview),"decision":try field(decision)])}
    public func recovery()async throws->[LibreSyncRecoverySnapshot]{try await request("recovery")}
    public func pruneRecovery(retain:Int)async throws{try await unit("prune_recovery",["retain":.number(Decimal(retain))])}
    public func pendingPairings()async throws->[LibreSyncEnrollment]{try await request("pending_pairings")}
    public func recoverPairing()async throws{try await unit("recover_pairing")}
    public func reportPermission(_ state:LibreSyncPermissionState)async throws{try await unit("evidence",["platform":.string("Apple"),"permission":.string("LocalNetwork"),"state":try field(state)])}
    public func platformAdvertisement()async throws->LibreSyncPlatformAdvertisement{try await request("platform_advertisement")}
    public func ingest(_ peer:LibreSyncNearbyDevice)async throws{try await unit("ingest_discovery",["peer":try field(peer)])}
    public func withdraw(deviceID:String)async throws{try await unit("withdraw_discovery",["device_id":.string(deviceID)])}
    public func events()->AsyncThrowingStream<LibreSyncSessionEvent,Error> {
        AsyncThrowingStream(bufferingPolicy:.bufferingNewest(256)) {continuation in
            let worker=Task {do{let subscription:UInt64=try await request("subscribe");defer{Task{let _:LibreSyncJSON?=try? await request("unsubscribe",fields:["subscription":.number(Decimal(subscription))])}};while !Task.isCancelled{let events:[LibreSyncSessionEvent]=try await request("events",fields:["timeout_ms":.number(200),"subscription":.number(Decimal(subscription))]);for event in events{continuation.yield(event)}};continuation.finish()}catch{continuation.finish(throwing:error)}}
            continuation.onTermination={_ in worker.cancel()}
        }
    }
}
