import Foundation
import PackageDescription

// The C module `{{C_MODULE}}` comes from one of two places. Next to this
// manifest, a prebuilt `{{C_MODULE}}.xcframework` (assembled from the native
// libraries, see the README) wins. Otherwise `Sources/{{C_MODULE}}` declares
// the generated header as a system library that links `{{LIBRARY}}` from the
// linker search path, e.g. `swift build -Xlinker -L/path/to/lib`.
let xcframework = "{{C_MODULE}}.xcframework"
let cModule: Target = FileManager.default.fileExists(atPath: "\(Context.packageDirectory)/\(xcframework)")
    ? .binaryTarget(name: "{{C_MODULE}}", path: xcframework)
    : .systemLibrary(name: "{{C_MODULE}}")

let package = Package(
    name: "{{MODULE}}",
    platforms: [.macOS(.v10_15), .iOS(.v13), .tvOS(.v13), .watchOS(.v6)],
    products: [
        .library(name: "{{MODULE}}", targets: ["{{MODULE}}"]),
    ],
    targets: [
        cModule,
        .target(name: "{{MODULE}}", dependencies: ["{{C_MODULE}}"]),
    ]
)
