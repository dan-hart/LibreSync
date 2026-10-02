import Foundation
import Darwin

// One in-flight delivery and at most256 pending device IDs, one in-flight cleanup slot, and one permission state. Repeated callbacks
// coalesce to the latest hint instead of creating an unbounded task chain.
@MainActor
final class LibreSyncDiscoveryDelivery {
    enum Change { case discovered(LibreSyncNearbyDevice), withdrawn(String), permission(LibreSyncPermissionState) }
    private enum Key:Hashable {case device(String),permission}
    private let apply:@MainActor (Change) async -> Bool
    private var pending:[Key:Change]=[:]
    private var worker:Task<Void,Never>?
    private var inFlight:Key?
    private var owned:Set<String>=[]
    var ownedCount:Int{owned.count}
    var pendingCount:Int {pending.count}
    init(apply:@escaping @MainActor (Change) async -> Bool){self.apply=apply}
    func submit(_ change:Change,deviceID:String){
        let key:Key
        if case .permission=change{key = .permission}else{key = .device(deviceID)}
        if case .withdrawn=change {
            if case .discovered?=pending[key]{pending.removeValue(forKey:key)}
            guard owned.contains(deviceID) || inFlight==key else{return}
        }
        let devices=pending.keys.filter{if case .device=$0{return true};return false}.count
        if key != .permission,pending[key]==nil,devices>=256 {
            guard case .withdrawn=change else{return}
            if let evicted=pending.first(where:{if case .discovered=$0.value{return true};return false})?.key{pending.removeValue(forKey:evicted)}
            // Remaining withdrawals belong only to at most256 successful
            // deliveries plus the single discovery currently in flight.
        }
        pending[key]=change
        guard worker==nil else{return}
        worker=Task {
            while let id=pending.first(where:{if case .withdrawn=$0.value{return true};return false})?.key ?? pending.keys.first,
                  let next=pending.removeValue(forKey:id){
                if case .discovered(let peer)=next, !owned.contains(peer.identity.device_id),owned.count>=256{continue}
                if case .withdrawn(let device)=next,!owned.contains(device){continue}
                inFlight=id
                if await apply(next){
                    switch next{case .discovered(let peer):owned.insert(peer.identity.device_id);case .withdrawn(let device):owned.remove(device);case .permission:break}
                }
                inFlight=nil
            }
            worker=nil
        }
    }
    func drain()async{await worker?.value}
    func discardAndDrain()async{
        pending=pending.filter{if case .withdrawn=$0.value{return true};return false}
        await drain()
        for id in owned.sorted(){if await apply(.withdrawn(id)){owned.remove(id)}}
    }
}

/// System Bonjour for a fixed Info.plist-declared service type. Runs on the main
/// run loop, while Session networking/storage always runs off the main thread.
@MainActor
public final class LibreSyncBonjour: NSObject, NetServiceDelegate, NetServiceBrowserDelegate {
    private let session:LibreSyncSession
    private var browser=NetServiceBrowser()
    private var published:NetService?
    private var services:[String:NetService]=[:]
    private var ids:[String:String]=[:]
    private var latestTXT:[String:Data]=[:]
    private var resolved:Set<String>=[]
    private var refresh:Task<Void,Never>?
    private var last:LibreSyncPlatformAdvertisement?
    private var active=false
    private var stopping:(id:UUID,task:Task<Void,Never>)?
    private let delivery:LibreSyncDiscoveryDelivery
    private var generation=UUID()
    deinit{refresh?.cancel();browser.stop();published?.stop();for service in services.values{service.stopMonitoring();service.stop()}}
    public init(session:LibreSyncSession){self.session=session;delivery=LibreSyncDiscoveryDelivery {change in do{switch change {case .discovered(let peer):try await session.ingest(peer);case .withdrawn(let id):try await session.withdraw(deviceID:id);case .permission(let state):try await session.reportPermission(state)};return true}catch{return false}};super.init();browser.delegate=self;browser.schedule(in:.main,forMode:.common)}
    public func start()async throws {
        await stop()
        active=true;generation=UUID()
        let token=generation
        try await update()
        guard active,token==generation else{return}
        browser=NetServiceBrowser();browser.delegate=self;browser.schedule(in:.main,forMode:.common)
        browser.searchForServices(ofType:"_libresync._tcp.",inDomain:"local.")
        refresh=Task {[weak self] in while !Task.isCancelled {try? await Task.sleep(nanoseconds:300_000_000);guard !Task.isCancelled,let owner=self else{break};do{try await owner.update()}catch{await owner.stop();break}}}
    }
    public func stop()async{
        if let stopping {await stopping.task.value;return}
        active=false;generation=UUID();refresh?.cancel();refresh=nil
        browser.stop();browser.delegate=nil;published?.stop();published=nil;last=nil
        for service in services.values{service.stopMonitoring();service.stop()}
        services.removeAll();ids.removeAll();latestTXT.removeAll();resolved.removeAll()
        let task=Task {await delivery.discardAndDrain()}
        let stopID=UUID();stopping=(stopID,task);await task.value
        if stopping?.id==stopID{stopping=nil}
    }
    private func update()async throws {
        let token=generation;let ad=try await session.platformAdvertisement();guard active,token==generation else{return}
        guard last?.port != ad.port || last?.txt != ad.txt else{return}
        // A changed descriptor replaces the publication. Retiring its delegate
        // also fences delayed callbacks from the previous published instance.
        published?.delegate=nil;published?.stop();published=nil
        let service=NetService(domain:"local.",type:"_libresync._tcp.",name:ad.instance,port:Int32(ad.port))
        service.delegate=self
        service.schedule(in:.main,forMode:.common)
        guard service.setTXTRecord(NetService.data(fromTXTRecord:ad.txt.mapValues{Data($0.utf8)})) else{service.stop();return}
        service.publish() // No .listenForConnections: Rust already owns the port.
        published=service
        last=ad
    }
    public func netServiceBrowser(_ browser:NetServiceBrowser,didFind service:NetService,moreComing:Bool){
        guard active,browser===self.browser,service.type=="_libresync._tcp.",services[service.name] != nil || services.count<256 else{return}
        if let previous=services[service.name]{
            if previous !== service{service.stopMonitoring();service.delegate=nil;service.stop()}
            return // Duplicate browse notifications must retain the live monitor.
        }
        services[service.name]=service;service.delegate=self;service.schedule(in:.main,forMode:.common);service.resolve(withTimeout:3)
    }
    public func netServiceBrowser(_ browser:NetServiceBrowser,didRemove service:NetService,moreComing:Bool){guard active,browser===self.browser else{return};let current=services.removeValue(forKey:service.name);current?.stopMonitoring();current?.stop();latestTXT.removeValue(forKey:service.name);resolved.remove(service.name);if let id=ids.removeValue(forKey:service.name){delivery.submit(.withdrawn(id),deviceID:id)}}
    public func netServiceDidResolveAddress(_ service:NetService){
        guard active,services[service.name]===service else{return}
        if resolved.insert(service.name).inserted{
            service.startMonitoring()
            if latestTXT[service.name]==nil,let seed=service.txtRecordData(),seed.count<=4096{latestTXT[service.name]=seed}
        }
        ingest(service,record:latestTXT[service.name])
    }
    public func netServiceDidPublish(_ service:NetService){guard active,published===service else{return};delivery.submit(.permission(.allowed),deviceID:"")}
    public func netService(_ service:NetService,didNotResolve errorDict:[String:NSNumber]){guard active,services[service.name]===service else{return};report(errorDict)}
    public func netService(_ service:NetService,didUpdateTXTRecord data:Data){
        guard active,services[service.name]===service,data.count<=4096 else{return}
        latestTXT[service.name]=data
        ingest(service,record:data)
    }
    public func netService(_ service:NetService,didNotPublish errorDict:[String:NSNumber]){guard active,published===service else{return};report(errorDict)}
    public func netServiceBrowser(_ browser:NetServiceBrowser,didNotSearch errorDict:[String:NSNumber]){guard active,browser===self.browser else{return};report(errorDict)}
    private func ingest(_ service:NetService,record:Data?=nil){
        guard active,services[service.name]===service else{return}
        guard let txt=record ?? service.txtRecordData(),txt.count<=4096 else{return}
        let values=NetService.dictionary(fromTXTRecord:txt).compactMapValues{String(data:$0,encoding:.utf8)}
        guard values["managed"]=="1",let app=values["app_id"],let device=values["device_id"],let user=values["user_id"],let name=values["name"],let kind=values["kind"],let role=values["role"],let appName=values["app_name"],let schema=values["schema"].flatMap(UInt32.init),let digest=values["contract"] else{return}
        if let existing=ids[service.name],existing != device{return}
        let addresses=(service.addresses ?? []).prefix(32).compactMap(Self.address)
        guard !addresses.isEmpty else{return}
        var descriptor:LibreSyncPairingDescriptor?
        if let id=values["pair_id"],let fp=values["pair_fp"],let expires=values["pair_expires"].flatMap(UInt64.init){descriptor=LibreSyncPairingDescriptor(version:values["pair_version"].flatMap(UInt32.init) ?? 1,invitation_id:id,inviter_fingerprint:fp,expires_at:expires)}
        let peer=LibreSyncNearbyDevice(identity:LibreSyncIdentity(device_id:device,user_id:user,app_id:app),addresses:addresses,advertisement:LibreSyncAdvertisement(display_name:name,device_kind:kind,role:role,app_display_name:appName,schema_version:schema,contract_digest:digest),invitation:descriptor)
        ids[service.name]=device
        delivery.submit(.discovered(peer),deviceID:device)
    }
    private func report(_ error:[String:NSNumber]) {
        // DNSService policy-denied is evidence; ordinary timeouts and failures are Unknown.
        let state:LibreSyncPermissionState=error[NetService.errorCode]?.intValue == -65570 ? .denied : .unknown
        delivery.submit(.permission(state),deviceID:"")
    }
    private static func address(_ data:Data)->String? {
        guard data.count>=MemoryLayout<sockaddr>.size else{return nil}
        return data.withUnsafeBytes{raw in
            guard let base=raw.baseAddress else{return nil};let socket=base.assumingMemoryBound(to:sockaddr.self)
            let family=Int32(socket.pointee.sa_family)
            guard (family==AF_INET && data.count>=MemoryLayout<sockaddr_in>.size) || (family==AF_INET6 && data.count>=MemoryLayout<sockaddr_in6>.size) else{return nil}
            var host=[CChar](repeating:0,count:Int(NI_MAXHOST));var port=[CChar](repeating:0,count:Int(NI_MAXSERV))
            guard getnameinfo(socket,socklen_t(data.count),&host,socklen_t(host.count),&port,socklen_t(port.count),NI_NUMERICHOST|NI_NUMERICSERV)==0 else{return nil}
            let h=String(cString:host)
            if family==AF_INET6 {let ipv6=base.assumingMemoryBound(to:sockaddr_in6.self).pointee;let numeric=h.split(separator:"%")[0];let scoped=ipv6.sin6_scope_id==0 ? String(numeric) : "\(numeric)%\(ipv6.sin6_scope_id)";return "[\(scoped)]:\(String(cString:port))"}
            return "\(h):\(String(cString:port))"
        }
    }
}
