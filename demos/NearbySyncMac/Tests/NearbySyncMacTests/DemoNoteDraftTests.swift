import XCTest
@testable import NearbySyncMac

final class DemoNoteDraftTests: XCTestCase {
    func testRemoteEditRefreshesAnUntouchedDraft() {
        var draft = DemoNoteDraft(note(title: "Original", body: "First"))
        draft.receive(note(title: "Remote", body: "Second", time: 2))
        XCTAssertEqual(draft.title, "Remote")
        XCTAssertEqual(draft.body, "Second")
        XCTAssertFalse(draft.sourceChanged)
    }

    func testRemoteEditPreservesUnsavedTextAndRequiresReview() {
        var draft = DemoNoteDraft(note(title: "Original", body: "First"))
        draft.body = "Unsaved local changes"
        draft.receive(note(title: "Remote", body: "Second", time: 2))
        XCTAssertEqual(draft.title, "Original")
        XCTAssertEqual(draft.body, "Unsaved local changes")
        XCTAssertTrue(draft.sourceChanged)
    }

    func testLoadingLatestExplicitlyReplacesTheConflictingDraft() {
        var draft = DemoNoteDraft(note(title: "Original", body: "First"))
        draft.body = "Unsaved local changes"
        let latest = note(title: "Remote", body: "Second", time: 2)
        draft.receive(latest)
        draft.load(latest)
        XCTAssertEqual(draft.title, "Remote")
        XCTAssertEqual(draft.body, "Second")
        XCTAssertFalse(draft.sourceChanged)
    }

    func testSavedDraftBecomesTheNewBaselineWithoutAConflict() {
        var draft = DemoNoteDraft(note(title: "Original", body: "First"))
        draft.title = "  Saved title  "
        draft.body = "Saved body"
        draft.receive(note(title: "Saved title", body: "Saved body", time: 2))
        XCTAssertFalse(draft.sourceChanged)
        XCTAssertEqual(draft.title, "Saved title")
        draft.receive(note(title: "Next remote title", body: "Remote body", time: 3))
        XCTAssertEqual(draft.title, "Next remote title")
        XCTAssertEqual(draft.body, "Remote body")
        XCTAssertFalse(draft.sourceChanged)
    }

    private func note(title: String, body: String, time: TimeInterval = 1) -> DemoNote {
        DemoNote(id: "note", title: title, body: body, updatedAt: Date(timeIntervalSince1970: time))
    }
}
