// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "NearbySyncMac",
    platforms: [.macOS(.v12)],
    dependencies: [.package(path: "../../bindings/swift")],
    targets: [
        .executableTarget(name: "NearbySyncMac", dependencies: [.product(name: "LibreSync", package: "swift")]),
        .testTarget(name: "NearbySyncMacTests", dependencies: ["NearbySyncMac"])
    ]
)
