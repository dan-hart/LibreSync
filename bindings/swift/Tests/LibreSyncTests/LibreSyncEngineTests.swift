import XCTest
import CLibreSync
@testable import LibreSync

/// Exercises the Swift wrapper against the universal `libresync-ffi` static
/// library: key generation, engine creation, listener, the event queue and
/// descriptor, and an asynchronous sync with a ticket.
final class LibreSyncEngineTests: XCTestCase {
    private func makeEngine(listen: Bool) throws -> (LibreSyncEngine, URL) {
        XCTAssertEqual(libresync_abi_version(), 2)
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("libresync-swift-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        guard let appKeyPtr = libresync_generate_app_key() else {
            XCTFail(LibreSyncEngine.lastError()); throw LibreSyncError.message("app key")
        }
        let appKey = String(cString: appKeyPtr)
        libresync_string_free(appKeyPtr)

        guard let keysPtr = libresync_generate_device_keys("swift-test", "com.example.swifttest", "tester") else {
            XCTFail(LibreSyncEngine.lastError()); throw LibreSyncError.message("device keys")
        }
        let keysJson = String(cString: keysPtr)
        libresync_string_free(keysPtr)
        let keys = try JSONDecoder().decode(GeneratedKeys.self, from: Data(keysJson.utf8))

        let config = LibreSyncConfig(
            deviceId: "swift-test",
            appId: "com.example.swifttest",
            userId: "tester",
            listenAddr: listen ? "127.0.0.1:0" : nil,
            appKey: appKey,
            deviceCertDer: keys.device_cert_der,
            deviceKeyDer: keys.device_key_der
        )
        let engine = try LibreSyncEngine(
            configJson: try config.jsonString(),
            statePath: dir.appendingPathComponent("state.enc").path
        )
        try engine.registerLogicalFileAdapter(
            id: "records",
            namespace: "com.example.swifttest",
            path: dir.appendingPathComponent("records.json").path
        )
        return (engine, dir)
    }

    private struct GeneratedKeys: Decodable {
        let device_cert_der: String
        let device_key_der: String
        let fingerprint: String
    }

    func testListenerEventsAndAsyncSync() throws {
        let (engine, _) = try makeEngine(listen: true)
        XCTAssertNil(engine.nextEvent())
        XCTAssertGreaterThanOrEqual(engine.eventFileDescriptor, 0)

        try engine.startListening()
        let started = engine.waitEvent(timeoutMs: 2000)
        XCTAssertEqual(started?.type, "listener_started")
        XCTAssertTrue((started?.fields["address"] as? String ?? "").hasPrefix("127.0.0.1:"))

        // A closed port: the async sync fails quickly and reports the ticket.
        let ticket = try engine.syncAsync(address: "127.0.0.1:1", adapterId: "records")
        XCTAssertGreaterThan(ticket, 0)
        var finished: LibreSyncEvent?
        for _ in 0..<50 {
            if let event = engine.waitEvent(timeoutMs: 200), event.type == "task_finished" {
                finished = event
                break
            }
        }
        XCTAssertEqual(finished?.ticket, ticket)
        XCTAssertEqual(finished?.succeeded, false)
        XCTAssertNotNil(finished?.errorMessage)
        XCTAssertFalse(engine.cancel(ticket: ticket))

        try engine.stopListening()
        XCTAssertEqual(engine.waitEvent(timeoutMs: 2000)?.type, "listener_stopped")
    }

    func testEventPumpDeliversOnQueue() throws {
        let (engine, _) = try makeEngine(listen: true)
        let expectation = expectation(description: "listener_started delivered by pump")
        let queue = DispatchQueue(label: "libresync.tests.pump")
        let pump = engine.makeEventPump(queue: queue) { event in
            if event.type == "listener_started" {
                expectation.fulfill()
            }
        }
        try engine.startListening()
        wait(for: [expectation], timeout: 5)
        withExtendedLifetime(pump) {}
        try engine.stopListening()
    }

    func testDeviceInfoDecodesFingerprint() throws {
        let json = #"{"device_id":"d","user_id":"u","app_id":"a","address":"127.0.0.1:1","linked":true,"fingerprint":"ff"}"#
        let info = try JSONDecoder().decode(LibreSyncDeviceInfo.self, from: Data(json.utf8))
        XCTAssertEqual(info.fingerprint, "ff")
        let event = LibreSyncEvent.decode(json: #"{"type":"sync_finished","device":\#(json),"result":{"ok":false,"error":"boom"}}"#)
        XCTAssertEqual(event?.device?.device_id, "d")
        XCTAssertEqual(event?.succeeded, false)
        XCTAssertEqual(event?.errorMessage, "boom")
    }
}
