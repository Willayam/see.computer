// swift-tools-version: 5.9
import PackageDescription

let artifacts = [
    ("executorch", "2145548f7210d2e2e67f9517536659b39f1a3451269df550c1c2326ce5ca679a"),
    ("backend_xnnpack", "fab4e8af45e8954f4e08c13cf000a208ba33adae974d5b26e1e99f03abecc08d"),
    ("kernels_optimized", "0ef3114ffe9bc37f4e34d65cc9e55df36fe0be96d558a3aec6f2f0a7bcdecaf7"),
    ("kleidiai", "b99d9674770c4eb7183c971ed852838a4a9194c909bdb54b28272c4e402d25c3"),
    ("threadpool", "df8087643fe7dcf2ee9aca9e25157102642eb214173240d28a92d5416ab23edd"),
]

let package = Package(
    name: "SwipeRuntime",
    platforms: [.iOS(.v17)],
    products: [.library(name: "SwipeRuntime", targets: ["SwipeRuntime"])],
    targets: artifacts.map { name, checksum in
        .binaryTarget(name: name, url: "https://ossci-ios.s3.amazonaws.com/executorch/\(name)-1.6.0.zip",
                      checksum: checksum)
    } + [.target(name: "SwipeRuntime", dependencies: artifacts.map { .target(name: $0.0) },
                 linkerSettings: [.linkedLibrary("c++"), .linkedFramework("Accelerate"),
                                  .linkedFramework("CoreImage"), .linkedFramework("CoreVideo")])]
)
