import Foundation
import CLibreSync

public struct LibreSyncAllowlistEntry: Codable {
    public let deviceId: String
    public let fingerprint: String

    public init(deviceId: String, fingerprint: String) {
        self.deviceId = deviceId
        self.fingerprint = fingerprint
    }

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case fingerprint
    }
}

public struct LibreSyncConfig: Codable {
    public let deviceId: String
    public let appId: String
    public let userId: String
    public let listenAddr: String?
    public let appKey: String
    public let deviceCertDer: String
    public let deviceKeyDer: String
    public let allowlist: [LibreSyncAllowlistEntry]
    public let autoAccept: Bool
    public let pairingSecret: String?

    public init(
        deviceId: String,
        appId: String,
        userId: String,
        listenAddr: String? = nil,
        appKey: String,
        deviceCertDer: String,
        deviceKeyDer: String,
        allowlist: [LibreSyncAllowlistEntry] = [],
        autoAccept: Bool = false,
        pairingSecret: String? = nil
    ) {
        self.deviceId = deviceId
        self.appId = appId
        self.userId = userId
        self.listenAddr = listenAddr
        self.appKey = appKey
        self.deviceCertDer = deviceCertDer
        self.deviceKeyDer = deviceKeyDer
        self.allowlist = allowlist
        self.autoAccept = autoAccept
        self.pairingSecret = pairingSecret
    }

    public func jsonString(pretty: Bool = false) throws -> String {
        let encoder = JSONEncoder()
        if pretty {
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        }
        let data = try encoder.encode(self)
        return String(decoding: data, as: UTF8.self)
    }

    enum CodingKeys: String, CodingKey {
        case deviceId = "device_id"
        case appId = "app_id"
        case userId = "user_id"
        case listenAddr = "listen_addr"
        case appKey = "app_key"
        case deviceCertDer = "device_cert_der"
        case deviceKeyDer = "device_key_der"
        case allowlist
        case autoAccept = "auto_accept"
        case pairingSecret = "pairing_secret"
    }
}

public struct LibreSyncKeyMaterial: Codable {
    public let appKey: String
    public let deviceCertDer: String
    public let deviceKeyDer: String
    public let fingerprint: String

    public init(appKey: String, deviceCertDer: String, deviceKeyDer: String, fingerprint: String) {
        self.appKey = appKey
        self.deviceCertDer = deviceCertDer
        self.deviceKeyDer = deviceKeyDer
        self.fingerprint = fingerprint
    }
}

public enum LibreSyncKeyManager {
    public static func loadOrCreate(
        deviceId: String,
        appId: String,
        userId: String,
        keychainService: String = "LibreSync"
    ) throws -> LibreSyncKeyMaterial {
        if let existing = try load(service: keychainService) {
            return existing
        }

        let appKey = try generateAppKey()
        let deviceKeys = try generateDeviceKeys(deviceId: deviceId, appId: appId, userId: userId)
        let material = LibreSyncKeyMaterial(
            appKey: appKey,
            deviceCertDer: deviceKeys.deviceCertDer,
            deviceKeyDer: deviceKeys.deviceKeyDer,
            fingerprint: deviceKeys.fingerprint
        )
        try save(material, service: keychainService)
        return material
    }

    public static func clear(keychainService: String = "LibreSync") throws {
        try LibreSyncKeychain.delete(account: "app_key", service: keychainService)
        try LibreSyncKeychain.delete(account: "device_cert_der", service: keychainService)
        try LibreSyncKeychain.delete(account: "device_key_der", service: keychainService)
        try LibreSyncKeychain.delete(account: "fingerprint", service: keychainService)
    }

    private static func load(service: String) throws -> LibreSyncKeyMaterial? {
        guard
            let appKey = try loadString(account: "app_key", service: service),
            let cert = try loadString(account: "device_cert_der", service: service),
            let key = try loadString(account: "device_key_der", service: service),
            let fingerprint = try loadString(account: "fingerprint", service: service)
        else {
            return nil
        }
        return LibreSyncKeyMaterial(appKey: appKey, deviceCertDer: cert, deviceKeyDer: key, fingerprint: fingerprint)
    }

    private static func save(_ material: LibreSyncKeyMaterial, service: String) throws {
        try saveString(account: "app_key", value: material.appKey, service: service)
        try saveString(account: "device_cert_der", value: material.deviceCertDer, service: service)
        try saveString(account: "device_key_der", value: material.deviceKeyDer, service: service)
        try saveString(account: "fingerprint", value: material.fingerprint, service: service)
    }

    private static func loadString(account: String, service: String) throws -> String? {
        guard let data = try LibreSyncKeychain.load(account: account, service: service) else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    private static func saveString(account: String, value: String, service: String) throws {
        guard let data = value.data(using: .utf8) else {
            throw LibreSyncError.message("invalid UTF-8 value for keychain")
        }
        try LibreSyncKeychain.save(data: data, account: account, service: service)
    }

    private struct FfiDeviceKeys: Codable {
        let deviceCertDer: String
        let deviceKeyDer: String
        let fingerprint: String

        enum CodingKeys: String, CodingKey {
            case deviceCertDer = "device_cert_der"
            case deviceKeyDer = "device_key_der"
            case fingerprint
        }
    }

    private static func generateAppKey() throws -> String {
        guard let ptr = libresync_generate_app_key() else {
            throw LibreSyncError.message(LibreSyncEngine.lastError())
        }
        defer { libresync_string_free(ptr) }
        return String(cString: ptr)
    }

    private static func generateDeviceKeys(deviceId: String, appId: String, userId: String) throws -> FfiDeviceKeys {
        guard let ptr = libresync_generate_device_keys(deviceId, appId, userId) else {
            throw LibreSyncError.message(LibreSyncEngine.lastError())
        }
        defer { libresync_string_free(ptr) }
        let json = String(cString: ptr)
        return try JSONDecoder().decode(FfiDeviceKeys.self, from: Data(json.utf8))
    }
}
