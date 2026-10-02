import Foundation
import Darwin

/// Example app-owned coherent records+receipts transaction. After this returns,
/// acknowledge that same inbox. Replace with your app database transaction when
/// integrating; never acknowledge after an unsuccessful durable app save.
public enum LibreSyncInboxJournal {
    public static func save(_ inbox:LibreSyncInbox,to destination:URL)throws {
        let directory=destination.deletingLastPathComponent()
        try FileManager.default.createDirectory(at:directory,withIntermediateDirectories:true)
        let temporary=directory.appendingPathComponent(".inbox-\(UUID().uuidString).tmp")
        defer{try? FileManager.default.removeItem(at:temporary)}
        try JSONEncoder().encode(inbox).write(to:temporary)
        let writer=try FileHandle(forWritingTo:temporary);try writer.synchronize();try writer.close()
        guard rename(temporary.path,destination.path)==0 else{throw POSIXError(POSIXErrorCode(rawValue:errno) ?? .EIO)}
        let fd=open(directory.path,O_RDONLY|O_DIRECTORY)
        guard fd>=0 else{throw POSIXError(POSIXErrorCode(rawValue:errno) ?? .EIO)}
        defer{Darwin.close(fd)}
        guard fsync(fd)==0 else{throw POSIXError(POSIXErrorCode(rawValue:errno) ?? .EIO)}
    }
}
