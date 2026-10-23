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
    @MainActor
    func testBackgroundPausesSessionAndForegroundReusesIt() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let keys = EphemeralKeys()
        var opens = 0
        let model = DemoModel(arguments: [], stateDirectory: { _ in directory }, openSession: { config, _ in
            opens += 1
            return try await LibreSyncSession.open(config, keys: keys)
        })
        await model.start()
        let session = try XCTUnwrap(model.sync?.session)
        await model.setActive(false)
        let paused = try await session.snapshot()
        XCTAssertEqual(paused.phase, .paused)
        await model.setActive(true)
        let resumed = try await session.snapshot()
        XCTAssertEqual(resumed.phase, .running)
        XCTAssertEqual(opens, 1, "Foreground must preserve the open session and identity")
        await model.stop()
    }

    @MainActor
    func testBackgroundDuringStartupLeavesNewSessionPaused() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let opening = expectation(description: "Opening suspended")
        let backgrounding = expectation(description: "Background requested")
        let gate = OpenGate()
        let keys = EphemeralKeys()
        let model = DemoModel(arguments: [], stateDirectory: { _ in directory }, openSession: { config, _ in
            opening.fulfill()
            await gate.wait()
            return try await LibreSyncSession.open(config, keys: keys)
        })
        let startup = Task { await model.start() }
        await fulfillment(of: [opening], timeout: 3)
        let background = Task { backgrounding.fulfill(); await model.setActive(false) }
        await fulfillment(of: [backgrounding], timeout: 3)
        gate.release()
        await startup.value
        await background.value
        let snapshot = try await XCTUnwrap(model.sync?.session).snapshot()
        XCTAssertEqual(snapshot.phase, .paused)
        XCTAssertFalse(model.busy)
        await model.stop()
    }

    @MainActor
    func testBackgroundDuringRenamePausesReplacementAndAllowsResume() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let reopening = expectation(description: "Rename open suspended")
        let backgrounding = expectation(description: "Background requested")
        let gate = OpenGate()
        let keys = EphemeralKeys()
        var opens = 0
        let model = DemoModel(arguments: [], stateDirectory: { _ in directory }, openSession: { config, _ in
            opens += 1
            if opens == 2 { reopening.fulfill(); await gate.wait() }
            return try await LibreSyncSession.open(config, keys: keys)
        })
        await model.start()
        let original = try XCTUnwrap(model.sync?.session)
        let identity = try await original.snapshot().identity.device_id
        for _ in 0..<100 where model.busy {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertFalse(model.busy)
        model.rename("Renamed phone")
        await fulfillment(of: [reopening], timeout: 3)
        let background = Task { backgrounding.fulfill(); await model.setActive(false) }
        await fulfillment(of: [backgrounding], timeout: 3)
        gate.release()
        await background.value
        let replacement = try XCTUnwrap(model.sync?.session)
        let paused = try await replacement.snapshot()
        XCTAssertEqual(paused.phase, .paused)
        XCTAssertEqual(paused.identity.device_id, identity)
        XCTAssertFalse(model.busy)
        await model.setActive(true)
        let resumed = try await replacement.snapshot()
        XCTAssertEqual(resumed.phase, .running)
        XCTAssertEqual(model.sync?.localDisplayName, "Renamed phone")
        do { _ = try await original.snapshot(); XCTFail("Replaced session remains open") }
        catch let error as LibreSyncSessionError { XCTAssertEqual(error.code, .closed) }
        await model.stop()
    }

}
