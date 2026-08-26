// Moves the pointer to a screen point and clicks it, so a script can pick a
// row in the glass panel. Coordinates are top-left-origin screen points, the
// same space screencapture -R uses.
// Usage: swift click-at.swift <x> <y> [move-only]
import CoreGraphics
import Foundation

guard CommandLine.arguments.count >= 3,
      let x = Double(CommandLine.arguments[1]),
      let y = Double(CommandLine.arguments[2])
else {
    FileHandle.standardError.write("usage: click-at.swift <x> <y> [move-only]\n".data(using: .utf8)!)
    exit(1)
}

let point = CGPoint(x: x, y: y)
let move = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved,
                   mouseCursorPosition: point, mouseButton: .left)
move?.post(tap: .cghidEventTap)
usleep(120_000)

if CommandLine.arguments.count < 4 {
    for type in [CGEventType.leftMouseDown, .leftMouseUp] {
        let event = CGEvent(mouseEventSource: nil, mouseType: type,
                            mouseCursorPosition: point, mouseButton: .left)
        event?.post(tap: .cghidEventTap)
        usleep(60_000)
    }
}
print("pointer at \(x), \(y)")
