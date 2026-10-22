import XCTest
import LibreSync
@testable import NearbySyncMac

final class EphemeralKeys: LibreSyncSecureStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
    func get(_ name: String) throws -> Data? { lock.lock(); defer { lock.unlock() }; return values[name] }
    func set(_ name: String, value: Data) throws { lock.lock(); defer { lock.unlock() }; values[name] = value }
    func delete(_ name: String) throws { lock.lock(); defer { lock.unlock() }; values[name] = nil }
}

@MainActor
private final class OpenGate {
    var continuation: CheckedContinuation<Void, Never>?
    func wait() async { await withCheckedContinuation { continuation = $0 } }
    func release() { continuation?.resume(); continuation = nil }
}

final class DemoLifecycleTests: XCTestCase {
    @MainActor
    func testStopWaitsForOpeningSessionAndClosesItBeforeReturning() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let opening = expectation(description: "Session open suspended")
        let stopping = expectation(description: "Shutdown requested")
        let gate = OpenGate()
        let keys = EphemeralKeys()
        var openedSession: LibreSyncSession?
        var didStop = false
        let model = DemoModel(arguments: ["--profile", "lifecycle"], stateDirectory: { _ in directory }, openSession: { config, _ in
            opening.fulfill()
            await gate.wait()
            let session = try await LibreSyncSession.open(config, keys: keys)
            openedSession = session
            return session
        })
        let startup = Task { await model.start() }
        await fulfillment(of: [opening], timeout: 3)
        let shutdown = Task {
            stopping.fulfill()
            await model.stop()
            didStop = true
        }
        await fulfillment(of: [stopping], timeout: 3)
        XCTAssertFalse(didStop, "Shutdown must wait for the outstanding native open")
        gate.release()
        await shutdown.value
        await startup.value
        XCTAssertTrue(didStop)
        XCTAssertNil(model.sync)
        let session = try XCTUnwrap(openedSession)
        do {
            _ = try await session.snapshot()
            XCTFail("Shutdown returned while the newly opened session was still usable")
        } catch let error as LibreSyncSessionError {
            XCTAssertEqual(error.code, .closed)
        }
    }
}
