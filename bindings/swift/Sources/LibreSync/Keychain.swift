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
        return result as? Data
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
