// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "LibreSync",
    products: [
        .library(name: "LibreSync", targets: ["LibreSync"]),
    ],
    targets: [
        .target(
            name: "CLibreSync",
            path: "Sources/CLibreSync",
            publicHeadersPath: "include"
        ),
        .target(
            name: "LibreSync",
            dependencies: ["CLibreSync"],
            path: "Sources/LibreSync"
        ),
    ]
)
