// swift-tools-version: 5.9
import PackageDescription
import Foundation

// Git consumers use exact immutable release bytes; source/CI opts into a built local framework.
let nativeBinary: Target = ProcessInfo.processInfo.environment["LIBRESYNC_USE_LOCAL_XCFRAMEWORK"] == "1"
    ? .binaryTarget(name: "LibreSyncFFI", path: "bindings/swift/LibreSyncFFI.xcframework")
    : .binaryTarget(name: "LibreSyncFFI", url: "https://github.com/dan-hart/LibreSync/releases/download/v0.8.0/LibreSyncFFI-0.8.0.xcframework.zip", checksum: "e5f0648de78468344cc35748538c925e2d43efc2b1d2a923d9efbb8ac3d478f8")

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
