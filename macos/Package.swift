// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Tyranny",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "Tyranny", targets: ["Tyranny"])],
    targets: [
        .target(name: "TyrannyCore"),
        .executableTarget(name: "Tyranny", dependencies: ["TyrannyCore"]),
        .testTarget(name: "TyrannyCoreTests", dependencies: ["TyrannyCore"]),
    ]
)
