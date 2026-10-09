import Foundation
import PackageDescription

{{SOURCE_COMMENT}}
let xcframework = "{{C_MODULE}}.xcframework"
let cModule: Target = FileManager.default.fileExists(atPath: "\(Context.packageDirectory)/\(xcframework)")
    ? .binaryTarget(name: "{{C_MODULE}}", path: xcframework)
    : {{FALLBACK}}

let package = Package(
    name: "{{MODULE}}",
    platforms: [{{PLATFORMS}}],
    products: [
        .library(name: "{{MODULE}}", targets: ["{{MODULE}}"]),
    ],
    targets: [
        cModule,
        .target(name: "{{MODULE}}", dependencies: ["{{C_MODULE}}"]),
    ]
)
