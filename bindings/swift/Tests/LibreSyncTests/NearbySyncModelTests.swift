import Network
import XCTest

@testable import LibreSync

final class NearbySyncModelTests: XCTestCase {
  @MainActor func testCompatibleDeviceDoesNotRequireOpenPairing() async throws {
    let session = try await makeSession("Compatibility")
    _ = try await session.start()
    let model = LibreSyncSessionModel(session: session)
    try await model.refresh()
    let ad = try await session.platformAdvertisement()
    let peer = LibreSyncNearbyDevice(
      identity: LibreSyncIdentity(device_id: "other", user_id: "user", app_id: "app"),
      addresses: [],
      advertisement: LibreSyncAdvertisement(
        display_name: "Nearby", device_kind: "desktop", role: "device", app_display_name: "Notes",
        schema_version: 1, contract_digest: ad.txt["contract"]!), invitation: nil)
    XCTAssertTrue(model.compatible(peer))
    XCTAssertFalse(model.canPair(peer))
    await session.close()
  }

  @MainActor func testCodePairingValidationAndDismissal() async throws {
    let source = try await makeSession("Source")
    let target = try await makeSession("Target")
    _ = try await source.start()
    _ = try await target.start()
    let sourceModel = LibreSyncSessionModel(session: source)
    try await sourceModel.refresh()
    sourceModel.beginPairing()
    sourceModel.offerCode()
    await sourceModel.waitForPendingAction()
    let invitation = try XCTUnwrap(sourceModel.invitation)
    let ad = try await source.platformAdvertisement()
    let peer = LibreSyncNearbyDevice(
      identity: invitation.identity, addresses: invitation.addresses,
      advertisement: LibreSyncAdvertisement(
        display_name: "Source", device_kind: "desktop", role: "device", app_display_name: "Notes",
        schema_version: 1, contract_digest: ad.txt["contract"]!),
      invitation: LibreSyncPairingDescriptor(
        version: 2, invitation_id: invitation.invitation.invitation_id,
        inviter_fingerprint: invitation.invitation.inviter_fingerprint,
        expires_at: invitation.invitation.expires_at))
    let model = LibreSyncSessionModel(session: target)
    try await model.refresh()
    model.beginPairing()
    model.connect(peer, code: "12x")
    XCTAssertNotNil(model.error)
    XCTAssertFalse(model.isBusy)
    let peersAfterInvalid = try await target.snapshot().peers
    XCTAssertTrue(peersAfterInvalid.isEmpty)
    let wrong = invitation.invitation.secret == "000000" ? "111111" : "000000"
    model.connect(peer, code: wrong)
    await model.waitForPendingAction()
    XCTAssertNotNil(model.error)
    let peersAfterWrong = try await target.snapshot().peers
    XCTAssertTrue(peersAfterWrong.isEmpty)
    model.connect(peer, code: invitation.invitation.secret)
    await model.waitForPendingAction()
    XCTAssertNil(model.error)
    let paired = try await target.snapshot().peers
    XCTAssertEqual(paired.count, 1)
    XCTAssertTrue(model.pairingSucceeded)
    try await sourceModel.refresh()
    XCTAssertTrue(sourceModel.pairingSucceeded)
    XCTAssertNil(sourceModel.invitation)
    await sourceModel.endPairingAndWait()
    await model.endPairingAndWait()
    await source.close()
    await target.close()
  }

  @MainActor func testInvitationDismissalFencesQueuedOfferAndPreservesError() async throws {
    let session = try await makeSession("Dismiss")
    _ = try await session.start()
    let model = LibreSyncSessionModel(session: session)
    model.beginPairing()
    model.offerCode()
    await model.endPairingAndWait()
    XCTAssertNil(model.invitation)
    let closedAdvertisement = try await session.platformAdvertisement()
    XCTAssertNil(closedAdvertisement.txt["pair_id"])
    XCTAssertFalse(model.isBusy)
    model.connect("invalid invitation")
    await model.waitForPendingAction()
    let error = model.error
    XCTAssertNotNil(error)
    await model.endPairingAndWait()
    XCTAssertEqual(model.error, error)
    await session.close()
  }

  @MainActor func testExpiredDescriptorCannotPairAndStartStopIsAwaitable() async throws {
    let session = try await makeSession("Lifecycle")
    let model = LibreSyncSessionModel(session: session)
    await model.startAndWait()
    let invitation = try await session.invitation(code: true)
    let ad = try await session.platformAdvertisement()
    let peer = LibreSyncNearbyDevice(
      identity: invitation.identity, addresses: invitation.addresses,
      advertisement: LibreSyncAdvertisement(
        display_name: "Other", device_kind: "desktop", role: "device", app_display_name: "Notes",
        schema_version: 1, contract_digest: ad.txt["contract"]!),
      invitation: LibreSyncPairingDescriptor(
        version: 2, invitation_id: invitation.invitation.invitation_id,
        inviter_fingerprint: invitation.invitation.inviter_fingerprint, expires_at: 1))
    XCTAssertTrue(model.compatible(peer))
    XCTAssertFalse(model.canPair(peer))
    XCTAssertEqual(model.localDisplayName, "Lifecycle")
    await model.stopAndWait()
    XCTAssertEqual(model.snapshot?.phase, .paused)
    XCTAssertNil(model.error)
    let phase = try await session.snapshot().phase
    XCTAssertEqual(phase, .paused)
    await session.close()
  }

  @MainActor func testStatusDistinguishesStoredFromApplied() {
    XCTAssertTrue(LibreSyncSessionModel.status(for: .stored).contains("waiting"))
    XCTAssertTrue(LibreSyncSessionModel.status(for: .upToDate).contains("applied"))
  }

  @MainActor func testDismissCancelsAnActualBlockedHandshake() async throws {
    let source = try await makeSession("Blocked source")
    let target = try await makeSession("Blocked target")
    _ = try await source.start()
    _ = try await target.start()
    let invitation = try await source.invitation()
    let accepted = expectation(description: "Native client connected to held handshake")
    let ready = expectation(description: "Held listener ready")
    let listener = try NWListener(using: .tcp, on: .any)
    listener.stateUpdateHandler = { state in if case .ready = state { ready.fulfill() } }
    let held = HeldConnection()
    listener.newConnectionHandler = { connection in
      held.keep(connection)
      connection.start(queue: .global())
      accepted.fulfill()
    }
    listener.start(queue: .global())
    await fulfillment(of: [ready], timeout: 3)
    let port = try XCTUnwrap(listener.port)
    let blocked = LibreSyncInvitation(
      invitation: invitation.invitation, identity: invitation.identity,
      addresses: ["127.0.0.1:\(port.rawValue)"])
    let model = LibreSyncSessionModel(session: target)
    model.beginPairing()
    model.connect(try blocked.encoded())
    await fulfillment(of: [accepted], timeout: 3)
    XCTAssertTrue(model.isBusy)
    let start = Date()
    await model.endPairingAndWait()
    XCTAssertLessThan(Date().timeIntervalSince(start), 3)
    XCTAssertFalse(model.isBusy)
    XCTAssertNil(model.error, "Intentional cancellation should not present a connection failure")
    let snapshot = try await target.snapshot()
    XCTAssertTrue(snapshot.peers.isEmpty)
    held.cancel()
    listener.cancel()
    await source.close()
    await target.close()
  }

  @MainActor func testRapidStartThenAwaitedStopRetiresObservation() async throws {
    let session = try await makeSession("Rapid lifecycle")
    var model: LibreSyncSessionModel? = LibreSyncSessionModel(session: session)
    weak var reference = model
    model?.start()
    model?.start()
    await model?.stopAndWait()
    let phase = try await session.snapshot().phase
    XCTAssertEqual(phase, .paused)
    model = nil
    for _ in 0..<20 {
      if reference == nil { break }
      try await Task.sleep(nanoseconds: 10_000_000)
    }
    XCTAssertNil(reference)
    await session.close()
  }

  @MainActor func testDuplicateOfferCannotReplaceInFlightCodeWithQR() async throws {
    let session = try await makeSession("Duplicate offer")
    _ = try await session.start()
    let model = LibreSyncSessionModel(session: session)
    model.beginPairing()
    model.offerCode()
    model.offerQR()
    await model.waitForPendingAction()
    let invitation = try XCTUnwrap(model.invitation)
    XCTAssertTrue(LibreSyncSessionModel.validCode(invitation.invitation.secret))
    let advertisement = try await session.platformAdvertisement()
    XCTAssertEqual(advertisement.txt["pair_id"], invitation.invitation.invitation_id)
    await model.endPairingAndWait()
    await session.close()
  }

  @MainActor func testSuccessfulExplicitRetryClearsPriorFailure() async throws {
    let session = try await makeSession("Retry")
    _ = try await session.start()
    let model = LibreSyncSessionModel(session: session)
    model.connect("invalid invitation")
    await model.waitForPendingAction()
    XCTAssertNotNil(model.error)
    await model.startAndWait()
    XCTAssertNil(model.error)
    await model.stopAndWait()
    await session.close()
  }

  @MainActor func testDefaultNameComesFromPersistentCoreIdentity() async throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    let keys = NearbyTestKeys()
    let first = try await LibreSyncSession.open(.notes(stateDirectory: directory), keys: keys)
    _ = try await first.start()
    let model = LibreSyncSessionModel(session: first)
    try await model.refresh()
    let name = model.localDisplayName
    XCTAssertFalse(name.isEmpty)
    XCTAssertNotEqual(name, "This device")
    await first.close()
    let reopened = try await LibreSyncSession.open(.notes(stateDirectory: directory), keys: keys)
    _ = try await reopened.start()
    let advertisement = try await reopened.platformAdvertisement()
    XCTAssertEqual(advertisement.txt["name"], name)
    await reopened.close()
  }

  private func makeSession(_ name: String) async throws -> LibreSyncSession {
    try await LibreSyncSession.open(
      .notes(
        stateDirectory: FileManager.default.temporaryDirectory.appendingPathComponent(
          UUID().uuidString), displayName: name), keys: NearbyTestKeys())
  }
}

private final class NearbyTestKeys: LibreSyncSecureStore, @unchecked Sendable {
  private let lock = NSLock()
  private var values: [String: Data] = [:]
  func get(_ name: String) throws -> Data? {
    lock.lock()
    defer { lock.unlock() }
    return values[name]
  }
  func set(_ name: String, value: Data) throws {
    lock.lock()
    defer { lock.unlock() }
    values[name] = value
  }
  func delete(_ name: String) throws {
    lock.lock()
    defer { lock.unlock() }
    values.removeValue(forKey: name)
  }
}

private final class HeldConnection: @unchecked Sendable {
  private let lock = NSLock()
  private var connection: NWConnection?
  func keep(_ value: NWConnection) {
    lock.lock()
    defer { lock.unlock() }
    connection = value
  }
  func cancel() {
    lock.lock()
    defer { lock.unlock() }
    connection?.cancel()
    connection = nil
  }
}
