import SwiftUI

/// Owns discovery and UI work for one open session. Hosts should await
/// `stopAndWait()` before closing or replacing the session.
@MainActor
public final class LibreSyncSessionModel: ObservableObject {
  public let session: LibreSyncSession
  @Published public private(set) var snapshot: LibreSyncSessionSnapshot?
  @Published public private(set) var nearby: [LibreSyncNearbyDevice] = []
  @Published public private(set) var invitation: LibreSyncInvitation?
  @Published public private(set) var bootstrap: LibreSyncBootstrapPreview?
  @Published public private(set) var error: String?
  @Published public private(set) var permission: LibreSyncPermissionState = .unknown
  @Published public private(set) var compatibleContract: String?
  @Published public private(set) var localDisplayName = "This device"
  @Published public private(set) var isBusy = false
  @Published public private(set) var isPairing = false
  @Published public private(set) var pairingSucceeded = false
  private var observer: Task<Void, Never>?
  private var lifecycle: Task<Void, Never>?
  private var action: Task<Void, Never>?
  private var pairingCleanup: Task<Void, Never>?
  private var pairingGeneration = UUID()
  private var pairingBaseline: Set<String>?
  private var lifecycleCount = 0
  private var lifecycleBusy: Bool { lifecycleCount > 0 }
  private let bonjour: LibreSyncBonjour
  private let permissionRequest = LibreSyncLocalNetworkPermission()

  public init(session: LibreSyncSession) {
    self.session = session
    bonjour = LibreSyncBonjour(session: session)
  }

  public func start() {
    sequence { [self] in
      let priorError = error
      do {
        _ = try await session.resume()
        try await bonjour.start()
        try await refresh()
        if error == priorError { error = nil }
        observer?.cancel()
        await observer?.value
        observer = Task { [weak self, session] in
          do {
            for try await event in session.events() {
              guard let owner = self else { break }
              owner.accept(event)
              try await owner.refresh()
            }
          } catch {
            if !Task.isCancelled { self?.error = error.localizedDescription }
          }
        }
      } catch { self.error = error.localizedDescription }
    }
  }

  public func startAndWait() async {
    start()
    await lifecycle?.value
  }
  public func stop() {
    // Fence actions immediately, before queued lifecycle work can yield.
    observer?.cancel()
    sequence { [self] in
      // A preceding queued start may have installed a new observer.
      observer?.cancel()
      await observer?.value
      observer = nil
      await endPairingAndWait()
      action?.cancel()
      await action?.value
      await bonjour.stop()
      do {
        try await session.pause()
        try await refresh()
      } catch { self.error = error.localizedDescription }
    }
  }
  public func stopAndWait() async {
    stop()
    await lifecycle?.value
  }
  public func refresh() async throws {
    let current = try await session.snapshot()
    let devices = try await session.nearby()
    snapshot = current
    nearby = devices
    if isPairing {
      let trusted = Set(current.peers.filter { !$0.revoked }.map { $0.identity.device_id })
      if let baseline = pairingBaseline {
        if !trusted.subtracting(baseline).isEmpty {
          pairingSucceeded = true
          invitation = nil
        }
      } else {
        pairingBaseline = trusted
      }
    }
    for event in current.diagnostics { accept(event) }
    guard current.phase == .running else { return }
    let advertisement = try await session.platformAdvertisement()
    compatibleContract = advertisement.txt["contract"]
    localDisplayName = advertisement.txt["name"] ?? "This device"
  }
  public func compatible(_ peer: LibreSyncNearbyDevice) -> Bool {
    guard let contract = compatibleContract, let advertisement = peer.advertisement else {
      return false
    }
    return advertisement.contract_digest == contract
  }
  public func canPair(_ peer: LibreSyncNearbyDevice, at date: Date = Date()) -> Bool {
    guard compatible(peer), let invitation = peer.invitation, invitation.version == 2,
      Double(invitation.expires_at) > date.timeIntervalSince1970
    else { return false }
    return !(snapshot?.peers.contains { $0.identity.device_id == peer.identity.device_id } ?? false)
  }
  public static func validCode(_ code: String) -> Bool {
    code.utf8.count == 6 && code.utf8.allSatisfy { (48...57).contains($0) }
  }
  public func beginPairing() {
    guard !isBusy, !lifecycleBusy, pairingCleanup == nil else { return }
    pairingGeneration = UUID()
    isPairing = true
    pairingSucceeded = false
    pairingBaseline = snapshot.map {
      Set($0.peers.filter { !$0.revoked }.map { $0.identity.device_id })
    }
  }
  public func endPairing() {
    guard pairingCleanup == nil else { return }
    isPairing = false
    pairingGeneration = UUID()
    invitation = nil
    action?.cancel()
    let pending = action
    isBusy = true
    pairingCleanup = Task { [self] in
      await pending?.value
      do { try await session.closePairing() } catch { self.error = error.localizedDescription }
      invitation = nil
      isBusy = false
      pairingCleanup = nil
    }
  }
  public func endPairingAndWait() async {
    endPairing()
    await pairingCleanup?.value
  }
  public func waitForPendingAction() async {
    await action?.value
    await pairingCleanup?.value
  }
  public func clearError() { error = nil }
  public func offerCode() { offer(code: true) }
  public func offerQR() { offer(code: false) }
  private func offer(code: Bool) {
    guard !isBusy, !lifecycleBusy else { return }
    if !isPairing { beginPairing() }
    let generation = pairingGeneration
    run { [self] in
      let value = try await session.invitation(code: code)
      if isPairing && generation == pairingGeneration { invitation = value }
    }
  }
  public func connect(_ encoded: String) {
    run { [self] in
      _ = try await session.connect(encoded)
      pairingSucceeded = true
      try await refresh()
    }
  }
  public func connect(_ peer: LibreSyncNearbyDevice, code: String) {
    guard !isBusy, !lifecycleBusy else { return }
    guard Self.validCode(code) else {
      error = "Enter all six digits shown on the other device."
      return
    }
    guard canPair(peer) else {
      error = "This invitation is unavailable or expired. Ask the other device to show a new code."
      return
    }
    run { [self] in
      _ = try await session.connect(peer, code: code)
      pairingSucceeded = true
      try await refresh()
    }
  }
  public func review(_ peer: LibreSyncPeer) {
    run { self.bootstrap = try await self.session.bootstrap(peer: peer.identity.device_id) }
  }
  public func resolve(_ decision: LibreSyncBootstrapDecision) {
    guard let preview = bootstrap else { return }
    run { [self] in
      try await session.resolve(preview, decision: decision)
      bootstrap = nil
      try await refresh()
    }
  }
  public func resume(_ peer: LibreSyncPeer) {
    run {
      try await self.session.resume(peer: peer.identity.device_id)
      try await self.refresh()
    }
  }
  public func pause(_ peer: LibreSyncPeer) {
    run {
      try await self.session.pause(peer: peer.identity.device_id)
      try await self.refresh()
    }
  }
  public func remove(_ peer: LibreSyncPeer) {
    run {
      try await self.session.remove(peer: peer.identity.device_id)
      try await self.refresh()
    }
  }
  public func repair(_ peer: LibreSyncPeer, invitation: String) {
    run {
      _ = try await self.session.repair(peer: peer.identity.device_id, invitation: invitation)
      try await self.refresh()
    }
  }
  public func wake() { start() }
  public func requestPermission() {
    permissionRequest.requestState { [weak self] in self?.setPermission($0) }
  }
  public func setPermission(_ state: LibreSyncPermissionState) {
    permission = state
    Task { [weak self, session] in
      do { try await session.reportPermission(state) } catch {
        self?.error = error.localizedDescription
      }
    }
  }
  private func accept(_ event: LibreSyncSessionEvent) {
    guard case .diagnostic(let diagnostic) = event,
      case .object(let root) = diagnostic.evidence,
      case .object(let platform)? = root["Platform"],
      platform["platform"] == .string("Apple"), platform["permission"] == .string("LocalNetwork"),
      case .string(let raw)? = platform["state"],
      let state = LibreSyncPermissionState(rawValue: raw)
    else { return }
    permission = state
  }
  private func sequence(_ operation: @escaping @MainActor () async -> Void) {
    lifecycleCount += 1
    let previous = lifecycle
    lifecycle = Task {
      await previous?.value
      await operation()
      lifecycleCount -= 1
    }
  }
  private func run(_ operation: @escaping @MainActor () async throws -> Void) {
    guard !isBusy, !lifecycleBusy else { return }
    isBusy = true
    error = nil
    action = Task { [self] in
      do {
        try Task.checkCancellation()
        try await operation()
      } catch is CancellationError {} catch let failure as LibreSyncSessionError
        where failure.code == .cancelled && Task.isCancelled
      {} catch { self.error = error.localizedDescription }
      if pairingCleanup == nil { isBusy = false }
      action = nil
    }
  }
  /// Technical evidence remains available as `error`; present this guidance first.
  public var errorMessage: String? {
    guard let error else { return nil }
    if error == "Enter all six digits shown on the other device."
      || error.hasPrefix("This invitation is unavailable")
    {
      return error
    }
    if isPairing {
      return "Couldn’t connect. Check the code or invitation, keep both apps open, and try again."
    }
    return
      "Nearby Sync couldn’t finish. Check connection help, then retry. Your saved data stays on this device."
  }
  public var phaseDescription: String {
    switch snapshot?.phase {
    case .running: return "Nearby Sync is on"
    case .paused: return "Nearby Sync is paused"
    case .failed: return "Nearby Sync needs attention"
    default: return "Nearby Sync is off"
    }
  }
  public static func status(for state: LibreSyncPeerState) -> String {
    switch state {
    case .waiting: return "Waiting for device"
    case .exchanging: return "Sending changes"
    case .stored: return "Saved on device — waiting for its app to apply changes"
    case .upToDate: return "Up to date — device confirmed applied changes"
    case .needsMerge: return "Review shared data to continue"
    case .needsRepair: return "Reconnect this device to continue"
    case .paused: return "Sync paused"
    }
  }
  deinit {
    observer?.cancel()
    action?.cancel()
  }
}
