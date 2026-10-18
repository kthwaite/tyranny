// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Tyranny",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "Tyranny", targets: ["Tyranny"])],
    targets: [.executableTarget(name: "Tyranny")]
)
