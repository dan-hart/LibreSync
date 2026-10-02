import Foundation
import Darwin

/// System Bonjour for a fixed Info.plist-declared service type. Runs on the main
/// run loop, while Session networking/storage always runs off the main thread.
@MainActor
public final class LibreSyncBonjour: NSObject, NetServiceDelegate, NetServiceBrowserDelegate {
    private let session:LibreSyncSession
    private let browser=NetServiceBrowser()
    private var published:NetService?
    private var services:[String:NetService]=[:]
    private var ids:[String:String]=[:]
    private var refresh:Task<Void,Never>?
    private var last:LibreSyncPlatformAdvertisement?
    private var active=false
    private var generation=UUID()
    deinit{refresh?.cancel();browser.stop();published?.stop();for service in services.values{service.stopMonitoring();service.stop()}}
    public init(session:LibreSyncSession){self.session=session;super.init();browser.delegate=self;browser.schedule(in:.main,forMode:.common)}
    public func start()async throws {
        await stop()
        active=true;generation=UUID()
        try await update()
        browser.searchForServices(ofType:"_libresync._tcp.",inDomain:"local.")
        refresh=Task {[weak self] in while !Task.isCancelled {try? await Task.sleep(nanoseconds:300_000_000);guard !Task.isCancelled,let owner=self else{break};do{try await owner.update()}catch{await owner.stop();break}}}
    }
    public func stop()async{active=false;generation=UUID();refresh?.cancel();refresh=nil;browser.stop();published?.stop();published=nil;last=nil;for service in services.values{service.stopMonitoring();service.stop()};services.removeAll();let withdrawn=Array(ids.values);ids.removeAll();for id in withdrawn{try? await session.withdraw(deviceID:id)}}
    private func update()async throws {
        let token=generation;let ad=try await session.platformAdvertisement();guard active,token==generation else{return}
        guard last?.port != ad.port || last?.txt != ad.txt else{return}
        if last?.port != ad.port || published==nil {
            published?.stop()
            let service=NetService(domain:"local.",type:"_libresync._tcp.",name:ad.instance,port:Int32(ad.port))
            service.delegate=self
            service.schedule(in:.main,forMode:.common)
            service.setTXTRecord(NetService.data(fromTXTRecord:ad.txt.mapValues{Data($0.utf8)}))
            service.publish() // No .listenForConnections: Rust already owns the port.
            published=service
        }else{published?.setTXTRecord(NetService.data(fromTXTRecord:ad.txt.mapValues{Data($0.utf8)}))}
        last=ad
    }
    public func netServiceBrowser(_ browser:NetServiceBrowser,didFind service:NetService,moreComing:Bool){
        
        guard active,services.count<256,service.type=="_libresync._tcp." else{return}
        services[service.name]=service;service.delegate=self;service.schedule(in:.main,forMode:.common);service.resolve(withTimeout:3)
    }
    public func netServiceBrowser(_ browser:NetServiceBrowser,didRemove service:NetService,moreComing:Bool){services.removeValue(forKey:service.name)?.stop();if let id=ids.removeValue(forKey:service.name){Task{try? await session.withdraw(deviceID:id)}}}
    public func netServiceDidResolveAddress(_ service:NetService){service.startMonitoring();ingest(service)}
    public func netServiceDidPublish(_ service:NetService){Task{try? await session.reportPermission(.allowed)}}
    public func netService(_ service:NetService,didNotResolve errorDict:[String:NSNumber]){report(errorDict)}
    public func netService(_ service:NetService,didUpdateTXTRecord data:Data){ingest(service,record:data)}
    public func netService(_ service:NetService,didNotPublish errorDict:[String:NSNumber]){report(errorDict)}
    public func netServiceBrowser(_ browser:NetServiceBrowser,didNotSearch errorDict:[String:NSNumber]){report(errorDict)}
    private func ingest(_ service:NetService,record:Data?=nil){
        guard active,services[service.name] != nil else{return}
        guard let txt=record ?? service.txtRecordData(),txt.count<=4096 else{return}
        let values=NetService.dictionary(fromTXTRecord:txt).compactMapValues{String(data:$0,encoding:.utf8)}
        guard values["managed"]=="1",let app=values["app_id"],let device=values["device_id"],let user=values["user_id"],let name=values["name"],let kind=values["kind"],let role=values["role"],let appName=values["app_name"],let schema=values["schema"].flatMap(UInt32.init),let digest=values["contract"] else{return}
        let addresses=(service.addresses ?? []).prefix(32).compactMap(Self.address)
        guard !addresses.isEmpty else{return}
        var descriptor:LibreSyncPairingDescriptor?
        if let id=values["pair_id"],let fp=values["pair_fp"],let expires=values["pair_expires"].flatMap(UInt64.init){descriptor=LibreSyncPairingDescriptor(version:values["pair_version"].flatMap(UInt32.init) ?? 1,invitation_id:id,inviter_fingerprint:fp,expires_at:expires)}
        let peer=LibreSyncNearbyDevice(identity:LibreSyncIdentity(device_id:device,user_id:user,app_id:app),addresses:addresses,advertisement:LibreSyncAdvertisement(display_name:name,device_kind:kind,role:role,app_display_name:appName,schema_version:schema,contract_digest:digest),invitation:descriptor)
        ids[service.name]=device
        let token=generation;Task{guard active,token==generation else{return};try? await session.ingest(peer)}
    }
    private func report(_ error:[String:NSNumber]) {
        // DNSService policy-denied is evidence; ordinary timeouts and failures are Unknown.
        let state:LibreSyncPermissionState=error[NetService.errorCode]?.intValue == -65570 ? .denied : .unknown
        Task{try? await session.reportPermission(state)}
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
