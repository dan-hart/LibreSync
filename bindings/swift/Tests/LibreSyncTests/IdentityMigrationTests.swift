import XCTest
@testable import LibreSync

final class IdentityMigrationTests: XCTestCase {
    func testMigrationTriesSortedStrictAccountsAndOnlySkipsMismatch() async throws {
        let a="managed-"+String(repeating:"a",count:64)+"-identity-v1"
        let b="managed-"+String(repeating:"b",count:64)+"-identity-v1"
        var attempts:[String?]=[]
        let result:Int = try await openWithIdentityMigration(enumerate:{[b,"other",a,a]},open:{ candidate in
            attempts.append(candidate)
            guard let candidate else{throw LibreSyncSessionError(code:.identityMigrationRequired,message:"migration")}
            if candidate==a{throw LibreSyncSessionError(code:.identityCandidateMismatch,message:"mismatch")}
            return 42
        })
        XCTAssertEqual(result,42)
        XCTAssertEqual(attempts,[nil,a,b])
    }
    func testOnlyMigrationRequiredEnumeratesAndStorageFailuresAbort() async throws {
        for failure in [LibreSyncSessionErrorCode.operationFailed,.storageUnavailable,.busy,.storageCommitUncertain] {
            var enumerated=false
            do {
                let _:Int=try await openWithIdentityMigration(enumerate:{enumerated=true;return []},open:{_ in throw LibreSyncSessionError(code:failure,message:"failure")})
                XCTFail("must throw")
            } catch let error as LibreSyncSessionError {XCTAssertEqual(error.code,failure)}
            XCTAssertFalse(enumerated)
        }
        var attempts=0
        do {
            let _:Int=try await openWithIdentityMigration(enumerate:{["managed-"+String(repeating:"a",count:64)+"-identity-v1"]},open:{candidate in
                attempts+=1
                throw LibreSyncSessionError(code:candidate==nil ? .identityMigrationRequired : .storageUnavailable,message:"failure")
            })
            XCTFail("must throw")
        } catch let error as LibreSyncSessionError {XCTAssertEqual(error.code,.storageUnavailable)}
        XCTAssertEqual(attempts,2)
    }
    func testEnumerationOverflowAbortsBeforeAnyCandidate() async throws {
        var attempts=0
        do {
            let _:Int=try await openWithIdentityMigration(enumerate:{(0...managedIdentityCandidateLimit).map{String(format:"managed-%064x-identity-v1",$0)}},open:{_ in
                attempts+=1
                throw LibreSyncSessionError(code:.identityMigrationRequired,message:"migration")
            })
            XCTFail("must throw")
        } catch let error as LibreSyncSessionError {XCTAssertEqual(error.code,.storageUnavailable)}
        XCTAssertEqual(attempts,1)
    }
    func testCustomStoreMigrationDoesNotEnumerate() async throws {
        do {
            let _:Int=try await openWithIdentityMigration(enumerate:nil,open:{_ in throw LibreSyncSessionError(code:.identityMigrationRequired,message:"migration")})
            XCTFail("must throw")
        } catch let error as LibreSyncSessionError {XCTAssertEqual(error.code,.identityMigrationRequired)}
    }
    func testCancellationAfterEnumerationPreventsCandidateOpen() async throws {
        let task=Task.detached { () -> Bool in
            var attempts=0
            do {
                let _:Int=try await openWithIdentityMigration(enumerate:{
                    withUnsafeCurrentTask{$0?.cancel()}
                    return ["managed-"+String(repeating:"a",count:64)+"-identity-v1"]
                },open:{_ in
                    attempts+=1
                    throw LibreSyncSessionError(code:.identityMigrationRequired,message:"migration")
                })
                return false
            }catch is CancellationError{return attempts==1}catch{return false}
        }
        let cancelled=await task.value
        XCTAssertTrue(cancelled)
    }

}
