// swift-tools-version: 5.9
import PackageDescription

// The Swift package links the Rust FFI crate as a universal (arm64 + x86_64)
// static library wrapped in an XCFramework. Build it first from the repo root:
//
//     scripts/build-macos-universal.sh
//
// which produces bindings/swift/LibreSyncFFI.xcframework. Consumers that pull
// this package from git can point `binaryTarget` at a release zip + checksum
// instead of the local path.
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
        .binaryTarget(
            name: "LibreSyncFFI",
            path: "LibreSyncFFI.xcframework"
        ),
        .target(
            name: "CLibreSync",
            path: "Sources/CLibreSync",
            publicHeadersPath: "include"
        ),
        .target(
            name: "LibreSync",
            dependencies: ["CLibreSync", "LibreSyncFFI"],
            path: "Sources/LibreSync",
            linkerSettings: [
                // Frameworks the Rust static library depends on (FSEvents via
                // `notify`, CoreFoundation). Static libraries do not carry
                // their framework dependencies, so declare them here.
                .linkedFramework("CoreServices"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("Security"),
            ]
        ),
        .testTarget(
            name: "LibreSyncTests",
            dependencies: ["LibreSync", "CLibreSync"],
            path: "Tests/LibreSyncTests"
        ),
    ]
)
