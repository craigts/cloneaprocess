// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "mac-runner-service",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(
            name: "RunnerService",
            targets: ["RunnerService"]
        ),
    ],
    targets: [
        .executableTarget(
            name: "RunnerService",
            path: "Sources/RunnerService"
        ),
    ]
)

