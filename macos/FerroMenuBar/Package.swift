// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "FerroMenuBar",
    platforms: [.macOS(.v15)],
    targets: [
        .executableTarget(
            name: "FerroMenuBar",
            path: "Sources/FerroMenuBar"
        )
    ]
)
