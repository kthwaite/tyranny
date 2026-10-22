// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "TyrannyLikesSmoke",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "tyranny likes smoke", targets: ["TyrannyLikesSmoke"])],
    targets: [
        .target(name: "TyrannyLikesSmokeCore"),
        .executableTarget(name: "TyrannyLikesSmoke", dependencies: ["TyrannyLikesSmokeCore"]),
        .testTarget(name: "TyrannyLikesSmokeCoreTests", dependencies: ["TyrannyLikesSmokeCore"]),
    ]
)
