import Foundation

/// Keeps received changes from silently becoming stale editor contents.
struct DemoNoteDraft {
    private(set) var id: String
    var title: String
    var body: String
    private(set) var sourceChanged = false
    private var original: DemoNote?

    init(_ note: DemoNote? = nil) {
        id = note?.id ?? UUID().uuidString
        title = note?.title ?? ""
        body = note?.body ?? ""
        original = note
    }

    mutating func load(_ note: DemoNote?) {
        self = DemoNoteDraft(note)
    }

    mutating func receive(_ latest: DemoNote?) {
        guard latest != original else {
            sourceChanged = false
            return
        }
        let isDirty = title != (original?.title ?? "") || body != (original?.body ?? "")
        let matchesSavedText = latest.map {
            $0.title == title.trimmingCharacters(in: .whitespacesAndNewlines) && $0.body == body
        } ?? false
        if !isDirty || matchesSavedText {
            load(latest)
        } else {
            sourceChanged = true
        }
    }
}
