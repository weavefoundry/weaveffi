// swift-tools-version:5.7
import PackageDescription

let package = Package(
    name: "WeaveFFIExample",
    platforms: [ .macOS(.v12) ],
    targets: [
        .executableTarget(name: "App", path: "Sources/App")
    ]
)
