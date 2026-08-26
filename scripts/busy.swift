// CPU hogs at a chosen scheduling class, so a benchmark can ask what dictation
// costs on a machine that is already busy. Usage: swift busy.swift [threads] [qos]
import Foundation

let threads = CommandLine.arguments.count > 1 ? Int(CommandLine.arguments[1]) ?? 8 : 8
let name = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "userInteractive"
let classes: [String: DispatchQoS.QoSClass] = [
    "userInteractive": .userInteractive,
    "userInitiated": .userInitiated,
    "default": .default,
    "utility": .utility,
]
let qos = classes[name] ?? .userInteractive

for _ in 0 ..< threads {
    DispatchQueue.global(qos: qos).async {
        var seed: UInt64 = 0x9E37_79B9_7F4A_7C15
        while true {
            seed = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            if seed == 0 { print(seed) }
        }
    }
}
print("busy: \(threads) threads at \(name)")
dispatchMain()
