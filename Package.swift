// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "md-orphan",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/apple/swift-argument-parser", from: "1.3.0"),
    ],
    targets: [
        .target(
            name: "MdOrphanLib",
            path: "Sources/Lib"
        ),
        .executableTarget(
            name: "md-orphan",
            dependencies: [
                "MdOrphanLib",
                .product(name: "ArgumentParser", package: "swift-argument-parser"),
            ],
            path: "Sources/CLI",
            swiftSettings: [
                .unsafeFlags(["-cross-module-optimization"], .when(configuration: .release)),
            ]
        ),
        .testTarget(
            name: "MdOrphanTests",
            dependencies: ["MdOrphanLib"],
            path: "Tests"
        ),
    ]
)
