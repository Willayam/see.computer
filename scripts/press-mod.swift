// Posts a bare modifier hold or a modifier+shift tap, for verifying see.computer's tap triggers.
// Usage: press-mod opt-hold [ms]   -> Left Option held (dictation)
//        press-mod opt-shift [ms]  -> Left Option held, Shift held for ms inside it (a clip past 250 ms)
//        press-mod opt-shot [ms]   -> Left Option held, one 100 ms Shift tap after 600 ms (a screenshot)
//        press-mod opt-tap [ms]    -> Left Option tapped for ms (default 80); two within 300 ms lock a take
//        press-mod opt-double-tap  -> two 80 ms Left Option taps 120 ms apart, in one process, so the 300 ms lock window is met
import Foundation
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2 else { FileHandle.standardError.write("usage: press-mod opt-hold|opt-shift|opt-shot|opt-tap|opt-double-tap|ropt-hold|ropt-shift [ms]\n".data(using:.utf8)!); exit(64) }
let mode = args[1]
let ms = args.count >= 3 ? (UInt32(args[2]) ?? 400) : 400
let leftOption: CGKeyCode = 58
let rightOption: CGKeyCode = 61
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
case "ropt-shift":
    let rf = CGEventFlags(rawValue: CGEventFlags.maskAlternate.rawValue | 0x40)
    let rfs = CGEventFlags(rawValue: CGEventFlags.maskAlternate.rawValue | CGEventFlags.maskShift.rawValue | 0x40 | 0x02)
    post(rightOption, down: true, flags: rf)
    usleep(30000)
    post(leftShift, down: true, flags: rfs)
    usleep(ms*1000)
    post(leftShift, down: false, flags: rf)
    post(rightOption, down: false, flags: [])
case "ropt-hold":
    // Right Option carries the device-specific right bit (0x40) so the L/R decode can be exercised.
    let rflags = CGEventFlags(rawValue: CGEventFlags.maskAlternate.rawValue | 0x40)
    post(rightOption, down: true, flags: rflags); usleep(ms*1000); post(rightOption, down: false, flags: [])
case "opt-shot":
    post(leftOption, down: true, flags: [.maskAlternate])
    usleep(600_000)
    post(leftShift, down: true, flags: [.maskAlternate, .maskShift])
    usleep(100_000)
    post(leftShift, down: false, flags: [.maskAlternate])
    usleep(ms*1000)
    post(leftOption, down: false, flags: [])
case "opt-double-tap":
    for _ in 0..<2 {
        post(leftOption, down: true, flags: [.maskAlternate]); usleep(80_000); post(leftOption, down: false, flags: [])
        usleep(120_000)
    }
case "opt-tap":
    let tapMs = args.count >= 3 ? ms : 80
    post(leftOption, down: true, flags: [.maskAlternate]); usleep(tapMs*1000); post(leftOption, down: false, flags: [])
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
