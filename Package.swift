// swift-tools-version: 5.9
import PackageDescription
import Foundation

// Git consumers use exact immutable release bytes; source/CI opts into a built local framework.
let nativeBinary: Target = ProcessInfo.processInfo.environment["LIBRESYNC_USE_LOCAL_XCFRAMEWORK"] == "1"
    ? .binaryTarget(name: "LibreSyncFFI", path: "bindings/swift/LibreSyncFFI.xcframework")
    : .binaryTarget(name: "LibreSyncFFI", url: "https://github.com/dan-hart/LibreSync/releases/download/v0.7.0/LibreSyncFFI-0.7.0.xcframework.zip", checksum: "8aff87c12416e9cb21bad37c4c4aec0520bd2fb7dff5c8050f8ae846af96d34d")

let package = Package(
    name: "LibreSync",
    platforms: [
        .macOS(.v12),
        .iOS(.v15),
    ],
    products: [
        .library(name: "LibreSync", targets: ["LibreSync"]),
    ],
    targets: [
        nativeBinary,
        .target(
            name: "CLibreSync",
            path: "bindings/swift/Sources/CLibreSync",
            publicHeadersPath: "include"
        ),
        .target(
            name: "LibreSync",
            dependencies: ["CLibreSync", "LibreSyncFFI"],
            path: "bindings/swift/Sources/LibreSync",
            linkerSettings: [
                // Frameworks the Rust static library depends on (FSEvents via
                // `notify`, CoreFoundation). Static libraries do not carry
                // their framework dependencies, so declare them here.
                .linkedFramework("CoreServices", .when(platforms: [.macOS])),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("Security"),
            ]
        ),
        .testTarget(
            name: "LibreSyncTests",
            dependencies: ["LibreSync", "CLibreSync"],
            path: "bindings/swift/Tests/LibreSyncTests"
        ),
    ]
)
