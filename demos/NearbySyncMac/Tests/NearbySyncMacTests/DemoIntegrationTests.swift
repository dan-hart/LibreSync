import XCTest
import LibreSync
@testable import NearbySyncMac

final class DemoIntegrationTests: XCTestCase {
    @MainActor
    private func waitFor(_ label: String, timeout: TimeInterval = 20,
                         _ predicate: @MainActor () async throws -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if try await predicate() { return }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        throw DemoError.message("Timed out waiting for \(label)")
    }

    @MainActor
    func testTwoDemoHostsDiscoverPairApplyRenameAndReopen() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let sourceDirectory = root.appendingPathComponent("source")
        let receiverDirectory = root.appendingPathComponent("receiver")
        let sourceKeys = EphemeralKeys()
        let receiverKeys = EphemeralKeys()
        func host(_ profile: String, directory: URL, keys: EphemeralKeys) -> DemoModel {
            DemoModel(arguments: ["--profile", profile], stateDirectory: { _ in directory },
                openSession: { config, _ in try await LibreSyncSession.open(config, keys: keys) })
        }
        let source = host("integration-source", directory: sourceDirectory, keys: sourceKeys)
        let receiver = host("integration-receiver", directory: receiverDirectory, keys: receiverKeys)
        var reopenedSource: DemoModel?
        var reopenedReceiver: DemoModel?
        do {
            await source.start(); await receiver.start()
            XCTAssertNil(source.error); XCTAssertNil(receiver.error)
            let sourceSync = try XCTUnwrap(source.sync)
            let receiverSync = try XCTUnwrap(receiver.sync)
            let sourceID = try await sourceSync.session.snapshot().identity.device_id
            let receiverID = try await receiverSync.session.snapshot().identity.device_id
            let initialName = sourceSync.localDisplayName
            XCTAssertNotEqual(initialName, "This device")
            let sourceSeedID = UUID().uuidString
            let receiverSeedID = UUID().uuidString
            // Checking readiness and submitting must share one MainActor turn:
            // the inbox poller may become busy across any awaited continuation.
            try await waitFor("both hosts submit initial notes") {
                guard !source.busy && !receiver.busy else { return false }
                source.save(id: sourceSeedID, title: "Source seed", body: "Temporary merge fixture")
                receiver.save(id: receiverSeedID, title: "Receiver seed", body: "Temporary merge fixture")
                return true
            }
            try await waitFor("each host durably saves its initial note") {
                source.notes.contains { $0.id == sourceSeedID } && receiver.notes.contains { $0.id == receiverSeedID }
            }
            let invitation = try await sourceSync.session.invitation(code: true, ttl: 120)
            try await waitFor("actual Bonjour discovery and current pairing descriptor") {
                try await receiverSync.session.nearby().contains {
                    $0.identity.device_id == sourceID && $0.advertisement?.display_name == initialName &&
                    $0.invitation?.invitation_id == invitation.invitation.invitation_id
                }
            }
            let nearby = try await receiverSync.session.nearby()
            let peer = try XCTUnwrap(nearby.first { $0.identity.device_id == sourceID })
            _ = try await receiverSync.session.connect(peer, code: invitation.invitation.secret)
            try await waitFor("authenticated enrollment on both hosts") {
                let a = try await sourceSync.session.snapshot()
                let b = try await receiverSync.session.snapshot()
                return a.peers.contains { $0.identity.device_id == receiverID } && b.peers.contains { $0.identity.device_id == sourceID }
            }
            try await waitFor("both initial bootstrap consent previews") {
                let sourcePreview = try await sourceSync.session.bootstrap(peer: receiverID)
                let receiverPreview = try await receiverSync.session.bootstrap(peer: sourceID)
                return sourcePreview != nil && receiverPreview != nil
            }
            // Consent uses the same SDK lifecycle and Session APIs as the native UI.
            // Pausing both runtimes makes each exact bootstrap preview stable.
            await sourceSync.stopAndWait(); await receiverSync.stopAndWait()
            let sourcePreview = try await sourceSync.session.bootstrap(peer: receiverID)
            let receiverPreview = try await receiverSync.session.bootstrap(peer: sourceID)
            try await sourceSync.session.resolve(XCTUnwrap(sourcePreview), decision: .combine)
            try await receiverSync.session.resolve(XCTUnwrap(receiverPreview), decision: .combine)
            await sourceSync.startAndWait(); await receiverSync.startAndWait()
            try await sourceSync.session.closePairing()
            try await waitFor("both hosts apply the explicitly combined notes") {
                source.notes.count == 2 && receiver.notes.count == 2
            }

            let noteID = UUID().uuidString
            try await waitFor("source host submits the integration note") {
                guard !source.busy else { return false }
                source.save(id: noteID, title: "Integration note", body: "Temporary end-to-end fixture")
                return true
            }
            do {
                try await waitFor("source app model applies its own saved note") {
                    source.notes.contains { $0.id == noteID && $0.title == "Integration note" }
                }
                try await waitFor("receiver app model applies the saved note") {
                    receiver.notes.contains { $0.id == noteID && $0.title == "Integration note" }
                }
            } catch {
                let sourceSnapshot = try await sourceSync.session.snapshot()
                let receiverSnapshot = try await receiverSync.session.snapshot()
                let sourcePeer = sourceSnapshot.peers.first { $0.identity.device_id == receiverID }
                let receiverPeer = receiverSnapshot.peers.first { $0.identity.device_id == sourceID }
                let sourceInbox = try await sourceSync.session.inbox()
                let receiverInbox = try await receiverSync.session.inbox()
                // Emit only numeric ports and allowlisted diagnostic categories.
                // Raw messages/evidence can contain addresses or application data.
                func ports(_ peers: [LibreSyncNearbyDevice], deviceID: String) -> [UInt16] {
                    Array(Set(peers.filter { $0.identity.device_id == deviceID }.flatMap { peer in
                        peer.addresses.compactMap { address in
                            address.split(separator: ":").last.flatMap { UInt16($0) }
                        }
                    })).sorted()
                }
                func diagnostics(_ snapshot: LibreSyncSessionSnapshot, peerID: String) -> [String] {
                    snapshot.diagnostics.compactMap { event -> String? in
                        guard case .diagnostic(let diagnostic) = event,
                              diagnostic.peer == nil || diagnostic.peer == peerID else { return nil }
                        let evidence: String
                        switch diagnostic.evidence {
                        case .string(let value) where ["Unknown", "StorageCommitUncertain", "NetworkError", "CertificateMismatch", "BootstrapRequired"].contains(value):
                            evidence = value
                        case .object(let value) where value["Platform"] != nil:
                            evidence = "Platform"
                        case .object(let value):
                            if case .object(let failure)? = value["ManagedFailure"],
                               case .string(let rawCode)? = failure["code"],
                               let code = LibreSyncSessionErrorCode(rawValue: rawCode) {
                                evidence = "ManagedFailure(\(code.rawValue))"
                            } else { evidence = "Unrecognized" }
                        default: evidence = "Unrecognized"
                        }
                        let message = diagnostic.message.lowercased()
                        let category: String
                        if message.contains("connection refused") { category = "ConnectionRefused" }
                        else if message.contains("timed out") || message.contains("timeout") { category = "Timeout" }
                        else if message.contains("peer destination not yet known") { category = "MissingDestination" }
                        else if message.contains("peer is revoked, paused or changed") || message.contains("cancelled") { category = "CancelledOrChanged" }
                        else if message.contains("tls") || message.contains("certificate") { category = "TLSOrCertificate" }
                        else if message.contains("expected authenticated managed hello") { category = "UnexpectedManagedHello" }
                        else if message.contains("expected managed delta") { category = "UnexpectedManagedDelta" }
                        else if message.contains("expected exact managed receipt") { category = "UnexpectedManagedReceipt" }
                        else { category = "Other" }
                        return "\(diagnostic.action.rawValue)/\(evidence)/\(category)"
                    }.suffix(8).map { $0 }
                }
                let sourcePort = try await sourceSync.session.platformAdvertisement().port
                let receiverPort = try await receiverSync.session.platformAdvertisement().port
                let sourceNearbyPorts = ports(try await sourceSync.session.nearby(), deviceID: receiverID)
                let receiverNearbyPorts = ports(try await receiverSync.session.nearby(), deviceID: sourceID)
                let status = [
                    "sourceHasNote=\(source.notes.contains { $0.id == noteID })",
                    "sourceCoreHasNote=\(sourceInbox.records.contains { $0.id == noteID })",
                    "sourceBusy=\(source.busy)",
                    "sourcePhase=\(sourceSnapshot.phase.rawValue)",
                    "sourceState=\(sourcePeer?.state.rawValue ?? "missing")",
                    "sourcePending=\(sourcePeer?.pending ?? 0)",
                    "sourceTransmitted=\(sourcePeer?.transmitted.sequence ?? 0)",
                    "receiverHasNote=\(receiver.notes.contains { $0.id == noteID })",
                    "receiverCoreHasNote=\(receiverInbox.records.contains { $0.id == noteID })",
                    "receiverBusy=\(receiver.busy)",
                    "receiverPhase=\(receiverSnapshot.phase.rawValue)",
                    "receiverState=\(receiverPeer?.state.rawValue ?? "missing")",
                    "receiverReceived=\(receiverPeer?.received.sequence ?? 0)",
                    "sourceListenerPort=\(sourcePort)",
                    "receiverListenerPort=\(receiverPort)",
                    "sourceNearbyPeerPorts=\(sourceNearbyPorts)",
                    "receiverNearbyPeerPorts=\(receiverNearbyPorts)",
                    "sourceNearbyMatchesReceiverListener=\(sourceNearbyPorts.contains(receiverPort))",
                    "receiverNearbyMatchesSourceListener=\(receiverNearbyPorts.contains(sourcePort))",
                    "sourceDiagnostics=\(diagnostics(sourceSnapshot, peerID: receiverID))",
                    "receiverDiagnostics=\(diagnostics(receiverSnapshot, peerID: sourceID))",
                    "sourceError=\(source.error != nil)",
                    "receiverError=\(receiver.error != nil)"
                ].joined(separator: ", ")
                throw DemoError.message("Note delivery failed: \(status); \(error.localizedDescription)")
            }
            let destination = receiverDirectory.appendingPathComponent("notes-inbox.json")
            let journal = try JSONDecoder().decode(LibreSyncInbox.self, from: Data(contentsOf: destination))
            XCTAssertTrue(try DemoInbox.notes(in: journal).contains { $0.id == noteID })
            let receipt = try XCTUnwrap(journal.receipts[sourceID])
            XCTAssertFalse(receipt.proof.isEmpty)
            var receiptStatus = "No peer snapshot"
            do {
                try await waitFor("sender sees the receiver's durable Applied receipt") {
                    let snapshot = try await sourceSync.session.snapshot()
                    guard let peer = snapshot.peers.first(where: { $0.identity.device_id == receiverID }) else { return false }
                    receiptStatus = "state=\(peer.state.rawValue), pending=\(peer.pending), applied=\(peer.applied.sequence), expected>=\(receipt.sequence), transmitted=\(peer.transmitted.sequence)"
                    return peer.applied.epoch == receipt.epoch && peer.applied.sequence >= receipt.sequence && peer.state == .upToDate
                }
            } catch {
                throw DemoError.message("Applied receipt did not converge: \(receiptStatus); sourceError=\(source.error != nil), receiverError=\(receiver.error != nil)")
            }
            // Sequence numbers and states are safe diagnostics; proof bytes and
            // invitation credentials never enter test output.
            print("Durable Applied convergence: \(receiptStatus)")
            let appliedJournal = try JSONDecoder().decode(LibreSyncInbox.self, from: Data(contentsOf: destination))
            let durableReceipt = try XCTUnwrap(appliedJournal.receipts[sourceID])
            let appliedSnapshot = try await sourceSync.session.snapshot()
            let appliedPeer = try XCTUnwrap(appliedSnapshot.peers.first { $0.identity.device_id == receiverID })
            XCTAssertEqual(appliedPeer.applied.epoch, durableReceipt.epoch)
            XCTAssertGreaterThanOrEqual(appliedPeer.applied.sequence, durableReceipt.sequence)
            XCTAssertGreaterThanOrEqual(durableReceipt.sequence, receipt.sequence)
            XCTAssertTrue(try DemoInbox.notes(in: appliedJournal).contains { $0.id == noteID })
            XCTAssertNil(source.error); XCTAssertNil(receiver.error)
            await source.setActive(false)
            let backgroundSnapshot = try await sourceSync.session.snapshot()
            XCTAssertEqual(backgroundSnapshot.phase, .paused)
            await source.setActive(true)
            let foregroundSnapshot = try await sourceSync.session.snapshot()
            XCTAssertEqual(foregroundSnapshot.identity.device_id, sourceID)
            XCTAssertTrue(foregroundSnapshot.peers.contains { $0.identity.device_id == receiverID && !$0.revoked })
            XCTAssertTrue(source.notes.contains { $0.id == noteID })
            try await waitFor("source host submits its rename") {
                guard !source.busy else { return false }
                source.rename("Renamed Integration Mac")
                return true
            }
            try await waitFor("source finishes stop/close/reopen rename") {
                !source.busy && source.sync?.localDisplayName == "Renamed Integration Mac"
            }
            let renamed = try XCTUnwrap(source.sync)
            let renamedID = try await renamed.session.snapshot().identity.device_id
            XCTAssertEqual(renamedID, sourceID)
            try await waitFor("authenticated trusted peer metadata refreshes after rename") {
                try await receiverSync.session.snapshot().peers.contains {
                    $0.identity.device_id == sourceID && $0.metadata.display_name == "Renamed Integration Mac"
                }
            }
            XCTAssertTrue(source.notes.contains { $0.id == noteID })
            XCTAssertTrue(receiver.notes.contains { $0.id == noteID })
            await source.stop(); await receiver.stop()

            let sourceAgain = host("integration-source", directory: sourceDirectory, keys: sourceKeys)
            let receiverAgain = host("integration-receiver", directory: receiverDirectory, keys: receiverKeys)
            reopenedSource = sourceAgain; reopenedReceiver = receiverAgain
            await sourceAgain.start(); await receiverAgain.start()
            XCTAssertNil(sourceAgain.error); XCTAssertNil(receiverAgain.error)
            let persistedSource = try XCTUnwrap(sourceAgain.sync)
            let persistedID = try await persistedSource.session.snapshot().identity.device_id
            XCTAssertEqual(persistedID, sourceID)
            let persistedSnapshot = try await persistedSource.session.snapshot()
            XCTAssertTrue(persistedSnapshot.peers.contains { $0.identity.device_id == receiverID && !$0.revoked })
            XCTAssertEqual(persistedSource.localDisplayName, "Renamed Integration Mac")
            XCTAssertTrue(sourceAgain.notes.contains { $0.id == noteID })
            XCTAssertTrue(receiverAgain.notes.contains { $0.id == noteID })
            await sourceAgain.stop(); await receiverAgain.stop()
        } catch {
            await source.stop(); await receiver.stop()
            await reopenedSource?.stop(); await reopenedReceiver?.stop()
            throw error
        }
    }
}
