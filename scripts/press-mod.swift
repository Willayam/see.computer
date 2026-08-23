// Posts a bare modifier hold or a modifier+shift tap, for verifying see.computer's tap triggers.
// Usage: press-mod opt-hold [ms]   -> Left Option held (dictation)
//        press-mod opt-shift [ms]  -> Left Option + Shift tap (video toggle)
import Foundation
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2 else { FileHandle.standardError.write("usage: press-mod opt-hold|opt-shift [ms]\n".data(using:.utf8)!); exit(64) }
let mode = args[1]
let ms = args.count >= 3 ? (UInt32(args[2]) ?? 400) : 400
let leftOption: CGKeyCode = 58
let leftShift: CGKeyCode = 56
guard let src = CGEventSource(stateID: .hidSystemState) else { exit(70) }
func post(_ key: CGKeyCode, down: Bool, flags: CGEventFlags) {
    guard let e = CGEvent(keyboardEventSource: src, virtualKey: key, keyDown: down) else { exit(70) }
    e.flags = flags
    e.post(tap: .cghidEventTap)
}
switch mode {
case "opt-hold":
    post(leftOption, down: true, flags: [.maskAlternate]); usleep(ms*1000); post(leftOption, down: false, flags: [])
case "opt-shift":
    post(leftOption, down: true, flags: [.maskAlternate])
    usleep(30000)
    post(leftShift, down: true, flags: [.maskAlternate, .maskShift])
    usleep(ms*1000)
    post(leftShift, down: false, flags: [.maskAlternate])
    post(leftOption, down: false, flags: [])
default:
    FileHandle.standardError.write("unknown mode\n".data(using:.utf8)!); exit(64)
}
print("posted \(mode) \(ms) ms")
