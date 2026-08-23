// Posts a see.computer hotkey chord as real HID keyboard events.
// Usage: swift scripts/press-hotkey.swift main [hold_ms]    (Alt+Space, held)
//        swift scripts/press-hotkey.swift video              (Cmd+Shift+Alt+Space, tap)
// Needs Accessibility permission for the process that runs it (Terminal, Codex).
import Foundation
import CoreGraphics

let args = CommandLine.arguments
guard args.count >= 2, ["main", "video"].contains(args[1]) else {
    FileHandle.standardError.write("usage: press-hotkey.swift main|video [hold_ms]\n".data(using: .utf8)!)
    exit(64)
}
let holdMs = args.count >= 3 ? UInt32(args[2]) ?? 1500 : (args[1] == "main" ? 1500 : 80)
let space: CGKeyCode = 49
let flags: CGEventFlags = args[1] == "main"
    ? [.maskAlternate]
    : [.maskCommand, .maskShift, .maskAlternate]
let modifierKeys: [(CGKeyCode, CGEventFlags)] = args[1] == "main"
    ? [(58, .maskAlternate)]
    : [(55, .maskCommand), (56, .maskShift), (58, .maskAlternate)]

guard let source = CGEventSource(stateID: .hidSystemState) else { exit(70) }
func post(_ key: CGKeyCode, down: Bool, flags: CGEventFlags) {
    guard let e = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: down) else { exit(70) }
    e.flags = flags
    e.post(tap: .cghidEventTap)
}
var held: CGEventFlags = []
for (key, flag) in modifierKeys { held.insert(flag); post(key, down: true, flags: held) }
post(space, down: true, flags: flags)
usleep(holdMs * 1000)
post(space, down: false, flags: flags)
for (key, _) in modifierKeys.reversed() { held.remove(modifierKeys.first { $0.0 == key }!.1); post(key, down: false, flags: held) }
print("posted \(args[1]) held \(holdMs) ms")
