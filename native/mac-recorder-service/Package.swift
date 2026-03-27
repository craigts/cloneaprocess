// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "mac-recorder-service",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(
            name: "RecorderService",
            targets: ["RecorderService"]
        ),
    ],
    targets: [
        .executableTarget(
            name: "RecorderService",
            path: "Sources/RecorderService"
        ),
    ]
)

