// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ExtPlugin",
    platforms: [.iOS(.v15)],
    products: [
        .library(name: "ExtPlugin", targets: ["ExtPlugin"])
    ],
    targets: [
        .target(name: "ExtPlugin")
    ]
)
