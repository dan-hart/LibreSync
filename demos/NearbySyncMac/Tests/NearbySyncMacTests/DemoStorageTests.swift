import XCTest
@testable import NearbySyncMac
import LibreSync

final class DemoStorageTests: XCTestCase {
    func testProfilesRejectPathAndNamespaceInjection() throws {
        for value in ["primary", "secondary", "smoke-primary", "a1"] {
            XCTAssertEqual(try DemoProfile(arguments: ["--profile", value]).name, value)
        }
        for value in ["", "../primary", "/tmp", "Primary", "a/b", String(repeating: "a", count: 33)] {
            XCTAssertThrowsError(try DemoProfile(arguments: ["--profile", value]))
        }
        XCTAssertThrowsError(try DemoProfile(arguments: ["--profile"]))
        XCTAssertThrowsError(try DemoProfile(arguments: ["--unknown"]))
    }

    func testJournalFailurePreventsAcknowledgement() async throws {
        let inbox = try JSONDecoder().decode(LibreSyncInbox.self, from: Data(#"{"revision":1,"records":[],"receipts":{}}"#.utf8))
        var acknowledged = false
        do {
            _ = try await DemoInbox.apply(inbox, to: URL(fileURLWithPath: "/dev/null/inbox.json")) { acknowledged = true }
            XCTFail("Saving in a non-directory must fail")
        } catch {}
        XCTAssertFalse(acknowledged)
    }

    func testInvalidNoteDoesNotReplaceAppDatabaseOrAcknowledge() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let destination = directory.appendingPathComponent("inbox.json")
        let existing = Data("existing database".utf8)
        try existing.write(to: destination)
        let json = #"{"revision":1,"records":[{"adapter":"notes","id":"bad","value":"e30=","deleted":false,"clock":{"counter":1,"device_id":"test"}}],"receipts":{}}"#
        let inbox = try JSONDecoder().decode(LibreSyncInbox.self, from: Data(json.utf8))
        var acknowledged = false
        do {
            _ = try await DemoInbox.apply(inbox, to: destination) { acknowledged = true }
            XCTFail("Malformed notes must fail before replacing the database")
        } catch {}
        XCTAssertFalse(acknowledged)
        XCTAssertEqual(try Data(contentsOf: destination), existing)
    }

    func testAcknowledgementSeesDurableRecordsAndReceipts() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let destination = directory.appendingPathComponent("inbox.json")
        let note = DemoNote(id: "note-id", title: "Test note", body: "Locally generated test", updatedAt: Date(timeIntervalSince1970: 1))
        let value = try JSONEncoder().encode(note).base64EncodedString()
        let json = """
        {"revision":4,"records":[{"adapter":"notes","id":"note-id","value":"\(value)","deleted":false,"clock":{"counter":1,"device_id":"test"}}],"receipts":{"peer":{"epoch":"epoch","sequence":2,"proof":"AQ=="}}}
        """
        let inbox = try JSONDecoder().decode(LibreSyncInbox.self, from: Data(json.utf8))
        let notes = try await DemoInbox.apply(inbox, to: destination) {
            let saved = try JSONDecoder().decode(LibreSyncInbox.self, from: Data(contentsOf: destination))
            let attributes = try FileManager.default.attributesOfItem(atPath: destination.path)
            XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
            XCTAssertEqual(saved.records, inbox.records)
            XCTAssertEqual(saved.receipts, inbox.receipts)
        }
        XCTAssertEqual(notes.map(\.id), ["note-id"])
    }
}
