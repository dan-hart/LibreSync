import XCTest
@testable import LibreSync
final class ManagedSessionTests:XCTestCase {
    func testAlreadyCancelledSetAndConnectCannotStartNativeWork() async throws {
        let a=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Cancel source"),keys:TestKeys())
        let b=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Cancel target"),keys:TestKeys())
        _ = try await a.start();_ = try await b.start()
        let invitation=try await a.invitation().encoded()
        let setGate=TestTaskGate()
        let set=Task.detached {setGate.wait();do{try await b.set(adapter:"records",id:"must-not-write",value:Data("cancelled".utf8));return false}catch is CancellationError{return true}catch{return false}}
        set.cancel();setGate.signal();let setCancelled=await set.value;XCTAssertTrue(setCancelled)
        let records=try await b.records(adapter:"records");XCTAssertTrue(records.isEmpty)
        let connectGate=TestTaskGate()
        let connect=Task.detached {connectGate.wait();do{_ = try await b.connect(invitation);return false}catch is CancellationError{return true}catch{return false}}
        connect.cancel();connectGate.signal();let connectCancelled=await connect.value;XCTAssertTrue(connectCancelled)
        let source=try await a.snapshot();let target=try await b.snapshot()
        XCTAssertTrue(source.peers.isEmpty);XCTAssertTrue(target.peers.isEmpty)
        let sourcePending=try await a.pendingPairings();let targetPending=try await b.pendingPairings()
        XCTAssertTrue(sourcePending.isEmpty);XCTAssertTrue(targetPending.isEmpty)
        await a.close();await b.close()
    }
    func testNearLimitInboxAcknowledgementDoesNotResendRecords() async throws {
        let s=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Compact acknowledgement"),keys:TestKeys())
        let clock=LibreSyncClock(counter:1,deviceID:String(repeating:"a",count:64))
        let records=[LibreSyncRecord(adapter:"records",id:"one",value:Data(repeating:1,count:64*1024*1024),clock:clock),LibreSyncRecord(adapter:"records",id:"two",value:Data(repeating:2,count:32*1024*1024-1100),clock:clock)]
        let receipts=Dictionary(uniqueKeysWithValues:(0..<10).map{("source-\($0)",LibreSyncReceipt(epoch:String(repeating:"e",count:128),sequence:1,proof:Data(repeating:3,count:32)))})
        let inbox=LibreSyncInbox(revision:0,records:records,receipts:receipts)
        let maximumCheckpoint=LibreSyncReceipt(epoch:String(repeating:"\0",count:128),sequence:UInt64.max,proof:Data(repeating:255,count:32))
        let maximumBatch=LibreSyncExportBatch(checkpoint:maximumCheckpoint,records:records,full:false)
        XCTAssertLessThanOrEqual(try JSONEncoder().encode(maximumBatch).count,128*1024*1024,"Aggregate fits the core full-batch budget including its maximum reserved checkpoint")
        let legacy=try JSONSerialization.data(withJSONObject:["op":"acknowledge_inbox","inbox":JSONSerialization.jsonObject(with:JSONEncoder().encode(inbox))])
        XCTAssertGreaterThan(legacy.count,128*1024*1024)
        do{try await s.acknowledge(inbox);XCTFail("Unknown source receipts must be refused")}catch let error as LibreSyncSessionError{XCTAssertEqual(error.code,.cancelled,"Compact metadata reaches history validation instead of hitting the request size cap")}
        let snapshot=try await s.snapshot();XCTAssertTrue(snapshot.peers.isEmpty)
        await s.close()
    }
    @MainActor func testObservableModelReleasesItsOwnedObservation() async throws {
        let session=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Model lifetime"),keys:TestKeys())
        var model:LibreSyncSessionModel?=LibreSyncSessionModel(session:session)
        weak var owner=model
        model?.start()
        try await Task.sleep(nanoseconds:300_000_000)
        model=nil
        for _ in 0..<20 {if owner==nil{break};try await Task.sleep(nanoseconds:50_000_000)}
        XCTAssertNil(owner,"Dropping model must release its observation and Bonjour provider")
        await session.close()
    }
    func testIndependentNativeObserversAndSubscriberCancellation() async throws {
        let s=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Observers"),keys:TestKeys())
        let first=Task {for try await event in s.events(){if case .changed=event{return true}};return false}
        let second=Task {for try await event in s.events(){if case .changed=event{return true}};return false}
        try await Task.sleep(nanoseconds:100_000_000)
        try await s.set(adapter:"records",id:"both",value:Data("event".utf8))
        let observedFirst=try await first.value;let observedSecond=try await second.value
        XCTAssertTrue(observedFirst);XCTAssertTrue(observedSecond)
        await s.close()
    }
    func testDurableNativeSessionsAndReceiptProofRoundtrip() async throws {
        let dir=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let s=try await LibreSyncSession.open(.notes(stateDirectory:dir,displayName:"Swift laptop"),keys:TestKeys())
        try await s.set(adapter:"records",id:"one",value:Data("hello".utf8))
        let inbox=try await s.inbox()
        XCTAssertEqual(inbox.records.first?.value,Data("hello".utf8))
        try await s.acknowledge(inbox)
        await s.close()
        do {_ = try await s.snapshot();XCTFail("closed handle accepted")}catch let e as LibreSyncSessionError {XCTAssertEqual(e.code,.closed)}
        let proof=Data((0..<32).map(UInt8.init))
        let receipt=LibreSyncReceipt(epoch:"epoch",sequence:7,proof:proof)
        XCTAssertEqual(try JSONDecoder().decode(LibreSyncReceipt.self,from:JSONEncoder().encode(receipt)).proof,proof)
    }
    func testNativeBonjourFindsActualFriendlyPeerAndInvitationRefresh() async throws {
        let a=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Bonjour A"),keys:TestKeys())
        let b=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Bonjour B"),keys:TestKeys())
        _ = try await a.start();_ = try await b.start()
        let providers=await MainActor.run{(LibreSyncBonjour(session:a),LibreSyncBonjour(session:b))}
        try await providers.0.start();try await providers.1.start()
        let deviceID=try await b.snapshot().identity.device_id
        var found=false
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B"}){found=true;break};try await Task.sleep(nanoseconds:100_000_000)}
        XCTAssertTrue(found,"system Bonjour should resolve actual Rust listener")
        let i=try await b.invitation(code:true)
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.invitation?.invitation_id==i.invitation.invitation_id}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let refreshed=try await a.nearby().contains(where:{$0.invitation?.invitation_id==i.invitation.invitation_id});XCTAssertTrue(refreshed)
        try await b.closePairing()
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B" && $0.invitation==nil}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let closedTXT=try await b.platformAdvertisement().txt;XCTAssertNil(closedTXT["pair_id"])
        let closed=try await a.nearby();XCTAssertTrue(closed.contains{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B" && $0.invitation==nil})
        let expiring=try await b.invitation(code:true,ttl:3)
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==deviceID && $0.invitation?.invitation_id==expiring.invitation.invitation_id}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let reopened=try await a.nearby().contains(where:{$0.identity.device_id==deviceID && $0.invitation?.invitation_id==expiring.invitation.invitation_id});XCTAssertTrue(reopened)
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==deviceID && $0.invitation==nil}), try await b.platformAdvertisement().txt["pair_id"]==nil {break};try await Task.sleep(nanoseconds:100_000_000)}
        let expiredTXT=try await b.platformAdvertisement().txt;XCTAssertNil(expiredTXT["pair_id"])
        let expired=try await a.nearby();XCTAssertTrue(expired.contains{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B" && $0.invitation==nil})
        await providers.1.stop()
        for _ in 0..<100 {if try await a.nearby().allSatisfy({$0.identity.device_id != deviceID}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let withdrawn=try await a.nearby();XCTAssertFalse(withdrawn.contains{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B"})
        try await b.pause();_ = try await b.resume();try await providers.1.start()
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B"}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let restarted=try await a.nearby();let realPort=try await b.platformAdvertisement().port
        XCTAssertTrue(restarted.contains{$0.identity.device_id==deviceID && $0.advertisement?.display_name=="Bonjour B" && $0.addresses.contains(where:{$0.hasSuffix(":\(realPort)")})})
        await providers.0.stop();await providers.1.stop();await a.close();await b.close()
    }
    func testNativePairBootstrapDurableInboxProofAndReconnect() async throws {
        let a=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Source"),keys:TestKeys())
        let b=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Receiver"),keys:TestKeys())
        try await a.set(adapter:"records",id:"source",value:Data("source".utf8))
        try await b.set(adapter:"records",id:"receiver",value:Data("receiver".utf8))
        _ = try await a.start();_ = try await b.start()
        let providers=await MainActor.run{(LibreSyncBonjour(session:a),LibreSyncBonjour(session:b))}
        try await providers.0.start();try await providers.1.start()
        let invite=try await a.invitation()
        _ = try await b.connect(invite.encoded())
        let aID=try await a.snapshot().identity.device_id;let bID=try await b.snapshot().identity.device_id
        for _ in 0..<100 {
            if try await a.bootstrap(peer:bID) != nil, try await b.bootstrap(peer:aID) != nil {break}
            try await Task.sleep(nanoseconds:100_000_000)
        }
        // Each merge changes the other device's incoming candidate. Quiesce both
        // owned runtimes before capturing the exact previews being approved.
        await providers.0.stop();await providers.1.stop()
        try await a.pause();try await b.pause()
        let aPhase=try await a.snapshot().phase;let bPhase=try await b.snapshot().phase
        XCTAssertEqual(aPhase,.paused);XCTAssertEqual(bPhase,.paused)
        let aBefore=try await a.records(adapter:"records");let bBefore=try await b.records(adapter:"records")
        XCTAssertEqual(aBefore.count,1);XCTAssertEqual(bBefore.count,1)
        let aCandidate=try await a.bootstrap(peer:bID);let bCandidate=try await b.bootstrap(peer:aID)
        let aPreview=try XCTUnwrap(aCandidate);let bPreview=try XCTUnwrap(bCandidate)
        try await a.resolve(aPreview,decision:.combine);try await b.resolve(bPreview,decision:.combine)
        _ = try await a.resume();_ = try await b.resume()
        try await providers.0.start();try await providers.1.start()
        for _ in 0..<100 {
            if try await b.records(adapter:"records").count==2, try await !b.inbox().receipts.isEmpty {break}
            try await Task.sleep(nanoseconds:100_000_000)
        }
        let inbox=try await b.inbox();XCTAssertEqual(inbox.records.count,2)
        XCTAssertFalse(inbox.receipts.isEmpty)
        XCTAssertTrue(inbox.receipts.values.allSatisfy{$0.proof.count==32})
        let sourceID=try await a.snapshot().identity.device_id
        let checkpoint=try XCTUnwrap(inbox.receipts[sourceID])
        let beforeSnapshot=try await a.snapshot();let before=try XCTUnwrap(beforeSnapshot.peers.first)
        XCTAssertNotEqual(before.applied,checkpoint)
        let obstruction=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data("file".utf8).write(to:obstruction)
        XCTAssertThrowsError(try LibreSyncInboxJournal.save(inbox,to:obstruction.appendingPathComponent("failed.json")))
        let failureSnapshot=try await a.snapshot();let afterFailure=try XCTUnwrap(failureSnapshot.peers.first)
        XCTAssertNotEqual(afterFailure.applied,checkpoint)
        let file=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString).appendingPathComponent("inbox.json")
        try LibreSyncInboxJournal.save(inbox,to:file)
        let saved=try JSONDecoder().decode(LibreSyncInbox.self,from:Data(contentsOf:file))
        XCTAssertEqual(saved.receipts,inbox.receipts)
        try await b.acknowledge(saved)
        for _ in 0..<100{if try await a.snapshot().peers.first?.applied==checkpoint{break};try await Task.sleep(nanoseconds:100_000_000)}
        let acknowledged=try await a.snapshot().peers.first?.applied
        XCTAssertEqual(acknowledged,checkpoint)
        await providers.1.stop();try await b.pause();_ = try await b.resume();try await providers.1.start()
        try await a.set(adapter:"records",id:"after-resume",value:Data("reconnect".utf8))
        for _ in 0..<100 {if try await b.records(adapter:"records").count==3{break};try await Task.sleep(nanoseconds:100_000_000)}
        let records=try await b.records(adapter:"records");XCTAssertEqual(records.count,3)
        let observer=Task{for try await _ in a.events(){}}
        observer.cancel();_ = try? await observer.value
        let state=try await a.snapshot().phase;XCTAssertEqual(state,.running)
        await providers.0.stop();await providers.1.stop();await a.close();await b.close()
    }
    func testQRPayloadAndUInt64PrecisionRoundtrip() throws {
        let payload="{\"invitation\":{\"version\":2},\"sample\":\"offline\"}"
        let image=try XCTUnwrap(LibreSyncQR.image(payload))
        XCTAssertEqual(LibreSyncQR.decode(image),payload)
        let data=Data("{\"sequence\":18446744073709551615}".utf8)
        let value=try JSONDecoder().decode(LibreSyncJSON.self,from:data)
        let encoded=try JSONEncoder().encode(value)
        let result=try JSONSerialization.jsonObject(with:encoded) as! [String:NSNumber]
        XCTAssertEqual(result["sequence"]?.uint64Value,UInt64.max)
    }
    func testSecureStoreUnavailableFailsWithoutRegeneration() async throws {
        let dir=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        do {_ = try await LibreSyncSession.open(.notes(stateDirectory:dir,displayName:"Locked"),keys:UnavailableKeys());XCTFail("unavailable secure store accepted")}catch let e as LibreSyncSessionError {XCTAssertEqual(e.code,.storageUnavailable)}
    }
}
private final class TestKeys:LibreSyncSecureStore,@unchecked Sendable {
    let lock=NSLock();var items:[String:Data]=[:]
    func get(_ name:String)throws->Data?{lock.lock();defer{lock.unlock()};return items[name]}
    func set(_ name:String,value:Data)throws{lock.lock();defer{lock.unlock()};items[name]=value}
    func delete(_ name:String)throws{lock.lock();defer{lock.unlock()};items.removeValue(forKey:name)}
}
private struct UnavailableKeys:LibreSyncSecureStore {
    func get(_ name:String)throws->Data?{throw NSError(domain:"locked",code:1)}
    func set(_ name:String,value:Data)throws{fatalError("must never regenerate")}
    func delete(_ name:String)throws{fatalError("must never delete")}
}

private final class TestTaskGate:@unchecked Sendable {
    private let semaphore=DispatchSemaphore(value:0)
    func wait(){semaphore.wait()}
    func signal(){semaphore.signal()}
}

extension ManagedSessionTests {
 @MainActor func testReplacedResolvedServiceCannotReplayClosedInvitation() async throws {
  let a=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Callback A"),keys:TestKeys())
  let b=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Callback B"),keys:TestKeys())
  _ = try await a.start();_ = try await b.start()
  let pa=LibreSyncBonjour(session:a);let pb=LibreSyncBonjour(session:b)
  try await pa.start();try await pb.start();let id=try await b.snapshot().identity.device_id
  _ = try await b.invitation(code:true);let ad=try await b.platformAdvertisement()
  for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==id&&$0.invitation != nil}){break};try await Task.sleep(nanoseconds:100_000_000)}
  let services=try XCTUnwrap(Mirror(reflecting:pa).children.first(where:{$0.label=="services"})?.value as? [String:NetService]);let old=try XCTUnwrap(services[id])
  try await b.closePairing()
  for _ in 0..<100 {if try await a.nearby().contains(where:{$0.identity.device_id==id&&$0.invitation==nil}){break};try await Task.sleep(nanoseconds:100_000_000)}
  let closed=try await a.nearby();XCTAssertTrue(closed.contains{$0.identity.device_id==id&&$0.invitation==nil})
  let replacement=NetService(domain:"local.",type:"_libresync._tcp.",name:id,port:Int32(ad.port))
  let browser=try XCTUnwrap(Mirror(reflecting:pa).children.first(where:{$0.label=="browser"})?.value as? NetServiceBrowser)
  pa.netServiceBrowser(browser,didRemove:old,moreComing:false)
  pa.netServiceBrowser(browser,didFind:replacement,moreComing:false)
  pa.netService(old,didUpdateTXTRecord:NetService.data(fromTXTRecord:ad.txt.mapValues{Data($0.utf8)}))
  let delivery=try XCTUnwrap(Mirror(reflecting:pa).children.first(where:{$0.label=="delivery"})?.value as? LibreSyncDiscoveryDelivery)
  await delivery.drain()
  let after=try await a.nearby();XCTAssertFalse(after.contains{$0.identity.device_id==id&&$0.invitation != nil},"Retired callback resurrected a closed invitation")
  await pa.stop();await pb.stop();await a.close();await b.close()
 }
}

extension ManagedSessionTests {
 @MainActor func testDiscoveryDeliveryCoalescesAndDrainsBeforeWithdrawal() async throws {
  let entered=expectation(description:"first delivery entered")
  var release:CheckedContinuation<Void,Never>?
  var delivered:[String]=[]
  let delivery=LibreSyncDiscoveryDelivery {change in
   guard case .discovered(let peer)=change else{return true}
   let id=peer.identity.device_id
   if id=="old"{await withCheckedContinuation{release=$0;entered.fulfill()}}
   delivered.append(id);return true
  }
  delivery.submit(.discovered(deliveryPeer("old")),deviceID:"peer")
  await fulfillment(of:[entered],timeout:2)
  for i in 0..<1000 {delivery.submit(.discovered(deliveryPeer("new-\(i)")),deviceID:"peer")}
  XCTAssertEqual(delivery.pendingCount,1)
  release?.resume();await delivery.drain()
  XCTAssertEqual(delivered,["old","new-999"])
 }
 @MainActor func testDiscoveryStopWaitsForInflightAndDiscardsQueuedHints() async throws {
  let entered=expectation(description:"first delivery entered")
  var release:CheckedContinuation<Void,Never>?
  var delivered:[String]=[]
  let delivery=LibreSyncDiscoveryDelivery {change in
   guard case .discovered(let peer)=change else{return true}
   let id=peer.identity.device_id
   await withCheckedContinuation{release=$0;entered.fulfill()};delivered.append(id);return true
  }
  delivery.submit(.discovered(deliveryPeer("old")),deviceID:"peer")
  await fulfillment(of:[entered],timeout:2)
  for i in 0..<1000 {delivery.submit(.discovered(deliveryPeer("queued-\(i)")),deviceID:"peer-\(i)")}
  delivery.submit(.permission(.unknown),deviceID:"")
  XCTAssertEqual(delivery.pendingCount,257)
  var drained=false
  let stop=Task{await delivery.discardAndDrain();drained=true}
  await Task.yield();XCTAssertFalse(drained)
  release?.resume();await stop.value
  XCTAssertTrue(drained);XCTAssertEqual(delivered,["old"]);XCTAssertEqual(delivery.pendingCount,0)
 }
}

extension ManagedSessionTests {
 @MainActor func testDiscoveryWithdrawalHasPriorityAtCapacity() async throws {
  let entered=expectation(description:"old hint in flight")
  var release:CheckedContinuation<Void,Never>?
  var values:[String]=[]
  let delivery=LibreSyncDiscoveryDelivery {change in
   switch change {
   case .discovered(let peer):
    if peer.identity.device_id=="old"{await withCheckedContinuation{release=$0;entered.fulfill()}}
    values.append("discovered:"+peer.identity.device_id)
   case .withdrawn(let id):values.append("withdrawn:"+id)
   case .permission:break
   }
   return true
  }
  func peer(_ id:String)->LibreSyncNearbyDevice{LibreSyncNearbyDevice(identity:LibreSyncIdentity(device_id:id,user_id:"u",app_id:"app"),addresses:[],advertisement:nil,invitation:nil)}
  delivery.submit(.discovered(peer("old")),deviceID:"old")
  await fulfillment(of:[entered],timeout:2)
  for i in 0..<256{delivery.submit(.discovered(peer("queued-\(i)")),deviceID:"queued-\(i)")}
  delivery.submit(.withdrawn("old"),deviceID:"old")
  XCTAssertEqual(delivery.pendingCount,256)
  release?.resume();await delivery.drain()
  XCTAssertEqual(Array(values.prefix(2)),["discovered:old","withdrawn:old"])
  XCTAssertEqual(values.count,257)
 }
}

extension ManagedSessionTests {
 @MainActor func testDiscoveryOwnedWithdrawalCannotBeDroppedByBogusWithdrawalCapacity() async throws {
  let entered=expectation(description:"blocking hint")
  var release:CheckedContinuation<Void,Never>?
  var withdrawn:[String]=[]
  let delivery=LibreSyncDiscoveryDelivery {change in
   switch change {
   case .discovered(let p):if p.identity.device_id=="block"{await withCheckedContinuation{release=$0;entered.fulfill()}}
   case .withdrawn(let id):withdrawn.append(id)
   case .permission:break
   }
   return true
  }
  func peer(_ id:String)->LibreSyncNearbyDevice{LibreSyncNearbyDevice(identity:LibreSyncIdentity(device_id:id,user_id:"u",app_id:"app"),addresses:[],advertisement:nil,invitation:nil)}
  delivery.submit(.discovered(peer("victim")),deviceID:"victim");await delivery.drain()
  delivery.submit(.discovered(peer("block")),deviceID:"block");await fulfillment(of:[entered],timeout:2)
  for i in 0..<256{delivery.submit(.withdrawn("unknown-\(i)"),deviceID:"unknown-\(i)")}
  delivery.submit(.withdrawn("victim"),deviceID:"victim")
  release?.resume();await delivery.drain()
  XCTAssertTrue(withdrawn.contains("victim"),"Owned hint withdrawal was dropped")
 }
 @MainActor func testDiscoveryOwnedWithdrawalSurvivesStopWhileQueued() async throws {
  let entered=expectation(description:"blocking hint")
  var release:CheckedContinuation<Void,Never>?
  var withdrawn:[String]=[]
  let delivery=LibreSyncDiscoveryDelivery {change in
   switch change {
   case .discovered(let p):if p.identity.device_id=="block"{await withCheckedContinuation{release=$0;entered.fulfill()}}
   case .withdrawn(let id):withdrawn.append(id)
   case .permission:break
   }
   return true
  }
  func peer(_ id:String)->LibreSyncNearbyDevice{LibreSyncNearbyDevice(identity:LibreSyncIdentity(device_id:id,user_id:"u",app_id:"app"),addresses:[],advertisement:nil,invitation:nil)}
  delivery.submit(.discovered(peer("victim")),deviceID:"victim");await delivery.drain()
  delivery.submit(.discovered(peer("block")),deviceID:"block");await fulfillment(of:[entered],timeout:2)
  delivery.submit(.withdrawn("victim"),deviceID:"victim")
  let stop=Task{await delivery.discardAndDrain()};await Task.yield()
  release?.resume();await stop.value
  XCTAssertTrue(withdrawn.contains("victim"),"Stop discarded an owned withdrawal")
 }
}

@MainActor private func deliveryPeer(_ id:String)->LibreSyncNearbyDevice{LibreSyncNearbyDevice(identity:LibreSyncIdentity(device_id:id,user_id:"u",app_id:"app"),addresses:[],advertisement:nil,invitation:nil)}

extension ManagedSessionTests {
 @MainActor func testFailedDiscoveryDoesNotOwnHintAndFailedCleanupRetriesOnLaterStop() async throws {
  var allowCleanup=false
  var withdrawals:[String]=[]
  let delivery=LibreSyncDiscoveryDelivery {change in
   switch change {
   case .discovered(let peer):return peer.identity.device_id != "denied"
   case .withdrawn(let id):withdrawals.append(id);return allowCleanup
   case .permission:return true
   }
  }
  delivery.submit(.discovered(deliveryPeer("denied")),deviceID:"denied");await delivery.drain()
  XCTAssertEqual(delivery.ownedCount,0)
  delivery.submit(.withdrawn("denied"),deviceID:"denied");await delivery.drain();XCTAssertTrue(withdrawals.isEmpty)
  delivery.submit(.discovered(deliveryPeer("owned")),deviceID:"owned");await delivery.drain()
  await delivery.discardAndDrain();XCTAssertEqual(delivery.ownedCount,1)
  allowCleanup=true;await delivery.discardAndDrain()
  XCTAssertEqual(delivery.ownedCount,0);XCTAssertEqual(withdrawals,["owned","owned"])
 }
}

private final class CachedTXTService:NetService {
 let cached:Data;let resolved:[Data]
 init(service:NetService,cached:Data){self.cached=cached;self.resolved=service.addresses ?? [];super.init(domain:service.domain,type:service.type,name:service.name,port:Int32(service.port))}
 override var addresses:[Data]?{resolved}
 override func txtRecordData()->Data?{cached}
}
extension ManagedSessionTests {
 @MainActor func testDuplicateFindKeepsTheMonitoredService() async throws {
  let s=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Duplicate find"),keys:TestKeys());_ = try await s.start()
  let p=LibreSyncBonjour(session:s);try await p.start()
  let browser=try XCTUnwrap(Mirror(reflecting:p).children.first(where:{$0.label=="browser"})?.value as? NetServiceBrowser)
  let first=NetService(domain:"local.",type:"_libresync._tcp.",name:"duplicate",port:12345)
  let duplicate=NetService(domain:"local.",type:"_libresync._tcp.",name:"duplicate",port:12345)
  p.netServiceBrowser(browser,didFind:first,moreComing:false);p.netServiceBrowser(browser,didFind:duplicate,moreComing:false)
  let tracked=try XCTUnwrap(Mirror(reflecting:p).children.first(where:{$0.label=="services"})?.value as? [String:NetService])
  XCTAssertTrue(tracked["duplicate"]===first,"Duplicate find retired a live monitoring owner")
  await p.stop();await s.close()
 }
 @MainActor func testResolveCacheCannotReplayAfterNewerTXTCallback() async throws {
  let a=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Cache A"),keys:TestKeys())
  let b=try await LibreSyncSession.open(.notes(stateDirectory:FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString),displayName:"Cache B"),keys:TestKeys())
  _ = try await a.start();_ = try await b.start();let pa=LibreSyncBonjour(session:a);let pb=LibreSyncBonjour(session:b);try await pa.start();try await pb.start()
  let id=try await b.snapshot().identity.device_id;_ = try await b.invitation(code:true);let oldAd=try await b.platformAdvertisement()
  for _ in 0..<100{if try await a.nearby().contains(where:{$0.identity.device_id==id&&$0.invitation != nil}){break};try await Task.sleep(nanoseconds:100_000_000)}
  let browser=try XCTUnwrap(Mirror(reflecting:pa).children.first(where:{$0.label=="browser"})?.value as? NetServiceBrowser)
  let tracked=try XCTUnwrap(Mirror(reflecting:pa).children.first(where:{$0.label=="services"})?.value as? [String:NetService]);let original=try XCTUnwrap(tracked[id])
  let delivery=try XCTUnwrap(Mirror(reflecting:pa).children.first(where:{$0.label=="delivery"})?.value as? LibreSyncDiscoveryDelivery)
  let stale=NetService.data(fromTXTRecord:oldAd.txt.mapValues{Data($0.utf8)});let cached=CachedTXTService(service:original,cached:stale)
  pa.netServiceBrowser(browser,didRemove:original,moreComing:false);pa.netServiceBrowser(browser,didFind:cached,moreComing:false);pa.netServiceDidResolveAddress(cached);await delivery.drain()
  try await b.closePairing();let closedAd=try await b.platformAdvertisement();pa.netService(cached,didUpdateTXTRecord:NetService.data(fromTXTRecord:closedAd.txt.mapValues{Data($0.utf8)}));await delivery.drain()
  let closed=try await a.nearby();XCTAssertTrue(closed.contains{$0.identity.device_id==id&&$0.invitation==nil})
  pa.netServiceDidResolveAddress(cached);await delivery.drain()
  let after=try await a.nearby();XCTAssertFalse(after.contains{$0.identity.device_id==id&&$0.invitation != nil},"Resolve cache replayed older invitation metadata")
  await pa.stop();await pb.stop();await a.close();await b.close()
 }
}
