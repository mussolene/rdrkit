// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "RDRKitApp",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "RDRKitCore", targets: ["RDRKitCore"]),
        .executable(name: "RDRKitApp", targets: ["RDRKitApp"]),
    ],
    targets: [
        .target(name: "RDRKitCore"),
        .executableTarget(
            name: "RDRKitApp",
            dependencies: ["RDRKitCore"]
        ),
        .testTarget(
            name: "RDRKitCoreTests",
            dependencies: ["RDRKitCore"]
        ),
    ]
)
