import Foundation
import Security

public enum LibreSyncKeychainError: Error, CustomStringConvertible {
    case status(OSStatus)

    public var description: String {
        switch self {
        case .status(let status): return "keychain error \(status)"
        }
    }
}

public enum LibreSyncKeychain {
    public static func save(data: Data, account: String, service: String = "LibreSync") throws {
        let baseQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: account,
            kSecAttrService as String: service,
        ]
        let updateFields: [String: Any] = [
            kSecValueData as String: data,
        ]
        let status = SecItemAdd(baseQuery.merging(updateFields, uniquingKeysWith: { _, new in new }) as CFDictionary, nil)
        if status == errSecSuccess {
            return
        }
        if status == errSecDuplicateItem {
            let updateStatus = SecItemUpdate(baseQuery as CFDictionary, updateFields as CFDictionary)
            if updateStatus != errSecSuccess {
                throw LibreSyncKeychainError.status(updateStatus)
            }
            return
        }
        throw LibreSyncKeychainError.status(status)
    }

    public static func load(account: String, service: String = "LibreSync") throws -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: account,
            kSecAttrService as String: service,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound {
            return nil
        }
        if status != errSecSuccess {
            throw LibreSyncKeychainError.status(status)
        }
        guard let data=result as? Data else{throw LibreSyncKeychainError.status(errSecDecode)}
        return data
    }

    /// Return account metadata only. Never request secret values during discovery.
    static func managedIdentityAccounts(service:String)throws->[String] {
        // Asking for one extra result makes truncation detectable without an unbounded query.
        let query:[String:Any]=[
            kSecClass as String:kSecClassGenericPassword,
            kSecAttrService as String:service,
            kSecReturnAttributes as String:true,
            kSecMatchLimit as String:managedIdentityCandidateLimit+1,
        ]
        var result:AnyObject?
        let status=SecItemCopyMatching(query as CFDictionary,&result)
        if status==errSecItemNotFound{return []}
        guard status==errSecSuccess else{throw LibreSyncSessionError(code:.storageUnavailable,message:"Legacy identity account metadata unavailable")}
        guard let items=result as? [[String:Any]],items.count<=managedIdentityCandidateLimit else{throw LibreSyncSessionError(code:.storageUnavailable,message:"Legacy identity account enumeration exceeded its bound")}
        var accounts:[String]=[]
        for item in items {
            guard let account=item[kSecAttrAccount as String] as? String,
                  let itemService=item[kSecAttrService as String] as? String,itemService==service else{throw LibreSyncSessionError(code:.storageUnavailable,message:"Invalid legacy identity account metadata")}
            accounts.append(account)
        }
        return try boundedManagedIdentityAccounts(accounts)
    }

    public static func delete(account: String, service: String = "LibreSync") throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: account,
            kSecAttrService as String: service,
        ]
        let status = SecItemDelete(query as CFDictionary)
        if status != errSecSuccess && status != errSecItemNotFound {
            throw LibreSyncKeychainError.status(status)
        }
    }
}
