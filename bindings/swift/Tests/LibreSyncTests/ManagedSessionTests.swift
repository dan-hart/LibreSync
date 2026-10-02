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
        var found=false
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.advertisement?.display_name=="Bonjour B"}){found=true;break};try await Task.sleep(nanoseconds:100_000_000)}
        XCTAssertTrue(found,"system Bonjour should resolve actual Rust listener")
        let i=try await b.invitation(code:true)
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.invitation?.invitation_id==i.invitation.invitation_id}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let refreshed=try await a.nearby().contains(where:{$0.invitation?.invitation_id==i.invitation.invitation_id});XCTAssertTrue(refreshed)
        try await b.closePairing()
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.advertisement?.display_name=="Bonjour B" && $0.invitation==nil}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let closed=try await a.nearby();XCTAssertTrue(closed.contains{$0.advertisement?.display_name=="Bonjour B" && $0.invitation==nil})
        _ = try await b.invitation(code:true,ttl:0.5)
        try await Task.sleep(nanoseconds:1_000_000_000)
        let expired=try await a.nearby();XCTAssertTrue(expired.contains{$0.advertisement?.display_name=="Bonjour B" && $0.invitation==nil})
        await providers.1.stop()
        for _ in 0..<100 {if try await a.nearby().allSatisfy({$0.advertisement?.display_name != "Bonjour B"}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let withdrawn=try await a.nearby();XCTAssertFalse(withdrawn.contains{$0.advertisement?.display_name=="Bonjour B"})
        try await b.pause();_ = try await b.resume();try await providers.1.start()
        for _ in 0..<100 {if try await a.nearby().contains(where:{$0.advertisement?.display_name=="Bonjour B"}){break};try await Task.sleep(nanoseconds:100_000_000)}
        let restarted=try await a.nearby();let realPort=try await b.platformAdvertisement().port
        XCTAssertTrue(restarted.contains{$0.advertisement?.display_name=="Bonjour B" && $0.addresses.contains(where:{$0.hasSuffix(":\(realPort)")})})
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
        for _ in 0..<100 {
            for session in [a,b] {for peer in try await session.snapshot().peers {if let preview=try await session.bootstrap(peer:peer.identity.device_id){try await session.resolve(preview,decision:.combine)}}}
            if try await b.records(adapter:"records").count==2 {break};try await Task.sleep(nanoseconds:100_000_000)
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
        try await b.pause();_ = try await b.resume()
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
