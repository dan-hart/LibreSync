import XCTest
import Foundation
import Darwin
@testable import LibreSync

private final class RecoveryKeys: LibreSyncSecureStore, @unchecked Sendable {
 private let lock=NSLock(); private var entries:[String:Data]=[:]
 func get(_ name:String)throws->Data?{lock.lock();defer{lock.unlock()};return entries[name]}
 func set(_ name:String,value:Data)throws{lock.lock();defer{lock.unlock()};entries[name]=value}
 func delete(_ name:String)throws{lock.lock();defer{lock.unlock()};entries.removeValue(forKey:name)}
}
private final class RecoveryService:NetService {
 var attempts=0; let cached:Data; var endpoints:[Data]; var nextEndpoints:[Data]?
 init(name:String,port:UInt16,txt:[String:String]) {
  cached=NetService.data(fromTXTRecord:txt.mapValues{Data($0.utf8)})
  var addr=sockaddr_in();addr.sin_len=UInt8(MemoryLayout<sockaddr_in>.size);addr.sin_family=sa_family_t(AF_INET);addr.sin_port=port.bigEndian;addr.sin_addr.s_addr=inet_addr("127.0.0.1")
  endpoints=[withUnsafeBytes(of:&addr){Data($0)}]
  super.init(domain:"local.",type:"_libresync._tcp.",name:name,port:Int32(port))
 }
 override var addresses:[Data]?{endpoints}
 override func txtRecordData()->Data?{cached}
 override func resolve(withTimeout timeout:TimeInterval){
  attempts+=1
  if attempts>1 {if let nextEndpoints {endpoints=nextEndpoints};Task { @MainActor [weak self] in guard let self else{return};self.delegate?.netServiceDidResolveAddress?(self)}}
 }
 override func startMonitoring(){}
 override func stopMonitoring(){}
 override func stop(){}
}
private func reserveLoopbackPort(_ port:UInt16)throws->Int32 {
 let descriptor=Darwin.socket(AF_INET,SOCK_STREAM,0)
 guard descriptor >= 0 else {throw NSError(domain:NSPOSIXErrorDomain,code:Int(errno))}
 var address=sockaddr_in();address.sin_len=UInt8(MemoryLayout<sockaddr_in>.size);address.sin_family=sa_family_t(AF_INET);address.sin_port=port.bigEndian;address.sin_addr.s_addr=inet_addr("127.0.0.1")
 let result=withUnsafePointer(to:&address){pointer in pointer.withMemoryRebound(to:sockaddr.self,capacity:1){Darwin.bind(descriptor,$0,socklen_t(MemoryLayout<sockaddr_in>.size))}}
 guard result == 0 else {let code=errno;Darwin.close(descriptor);throw NSError(domain:NSPOSIXErrorDomain,code:Int(code))}
 return descriptor
}
final class BonjourRecoveryTests:XCTestCase {
 @MainActor func testFailedResolveEventuallyRecoversWithoutBrowserRestart()async throws {
  let root=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
  addTeardownBlock {try? FileManager.default.removeItem(at:root)}
  let a=try await LibreSyncSession.open(.notes(stateDirectory:root.appendingPathComponent("a")),keys:RecoveryKeys())
  addTeardownBlock {await a.close()}
  let b=try await LibreSyncSession.open(.notes(stateDirectory:root.appendingPathComponent("b")),keys:RecoveryKeys())
  addTeardownBlock {await b.close()}
  _ = try await a.start();_ = try await b.start()
  let p=LibreSyncBonjour(session:a);addTeardownBlock {await p.stop()};try await p.start()
  let browser=try XCTUnwrap(Mirror(reflecting:p).children.first{$0.label=="browser"}?.value as? NetServiceBrowser)
  let ad=try await b.platformAdvertisement();let id=try await b.snapshot().identity.device_id
  let original=RecoveryService(name:id,port:ad.port,txt:ad.txt)
  p.netServiceBrowser(browser,didFind:original,moreComing:false)
  p.netService(original,didNotResolve:[NetService.errorCode:NSNumber(value:-72007)])
  let replacement=RecoveryService(name:id,port:ad.port,txt:ad.txt)
  p.netServiceBrowser(browser,didFind:replacement,moreComing:false)
  try await Task.sleep(nanoseconds:7_000_000_000)
  let nearby=try await a.nearby()
  print("BONJOUR_RECOVERY failure originalAttempts=\(original.attempts) replacementAttempts=\(replacement.attempts) peerPresent=\(nearby.contains{$0.identity.device_id==id})")
  XCTAssertTrue(nearby.contains{$0.identity.device_id==id && $0.addresses.contains("127.0.0.1:\(ad.port)")},"A transient resolution timeout must recover the real peer without restarting discovery")
 }
 @MainActor func testChangedListenerPortEventuallyRefreshesWithoutBrowserRemoval()async throws {
  let root=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
  addTeardownBlock {try? FileManager.default.removeItem(at:root)}
  let a=try await LibreSyncSession.open(.notes(stateDirectory:root.appendingPathComponent("a")),keys:RecoveryKeys())
  addTeardownBlock {await a.close()}
  let b=try await LibreSyncSession.open(.notes(stateDirectory:root.appendingPathComponent("b")),keys:RecoveryKeys())
  addTeardownBlock {await b.close()}
  _ = try await a.start();_ = try await b.start()
  let p=LibreSyncBonjour(session:a);addTeardownBlock {await p.stop()};try await p.start()
  let browser=try XCTUnwrap(Mirror(reflecting:p).children.first{$0.label=="browser"}?.value as? NetServiceBrowser)
  let delivery=try XCTUnwrap(Mirror(reflecting:p).children.first{$0.label=="delivery"}?.value as? LibreSyncDiscoveryDelivery)
  let oldAd=try await b.platformAdvertisement();let id=try await b.snapshot().identity.device_id
  let original=RecoveryService(name:id,port:oldAd.port,txt:oldAd.txt)
  p.netServiceBrowser(browser,didFind:original,moreComing:false);p.netServiceDidResolveAddress(original);await delivery.drain()
  let initial=try await a.nearby();XCTAssertTrue(initial.contains{$0.identity.device_id==id && $0.addresses.contains("127.0.0.1:\(oldAd.port)")})
  try await b.pause()
  // Occupy the released port so the real listener must bind a different one.
  // Ephemeral allocation is otherwise allowed to reuse its previous port.
  let reserved=try reserveLoopbackPort(oldAd.port)
  defer {Darwin.close(reserved)}
  _ = try await b.resume();let newAd=try await b.platformAdvertisement()
  XCTAssertNotEqual(oldAd.port,newAd.port)
  let replacement=RecoveryService(name:id,port:newAd.port,txt:newAd.txt)
  original.nextEndpoints=replacement.endpoints
  p.netServiceBrowser(browser,didFind:replacement,moreComing:false)
  p.netService(original,didUpdateTXTRecord:NetService.data(fromTXTRecord:newAd.txt.mapValues{Data($0.utf8)}))
  await delivery.drain();try await Task.sleep(nanoseconds:7_000_000_000)
  let nearby=try await a.nearby();let ports=nearby.filter{$0.identity.device_id==id}.flatMap{$0.addresses.compactMap{UInt16($0.split(separator:":").last ?? "")}}
  print("BONJOUR_RECOVERY replacement oldPort=\(oldAd.port) realNewPort=\(newAd.port) nearbyPorts=\(ports) originalAttempts=\(original.attempts) replacementAttempts=\(replacement.attempts)")
  XCTAssertTrue(ports.contains(newAd.port),"A live real peer listener port change must eventually replace its stale public nearby endpoint")
  XCTAssertFalse(ports.contains(oldAd.port))
 }
}
