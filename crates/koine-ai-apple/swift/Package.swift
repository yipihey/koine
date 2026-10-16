// swift-tools-version: 6.2
import PackageDescription

// The Foundation Models bridge `koine-ai-apple` links statically. `build.rs`
// runs `swift build -c release --product KoineFM`.
let package = Package(
    name: "KoineFM",
    platforms: [.macOS(.v26)],
    products: [.library(name: "KoineFM", type: .static, targets: ["KoineFM"])],
    targets: [.target(name: "KoineFM")]
)
