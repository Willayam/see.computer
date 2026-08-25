// Clicks see.computer's menu-bar icon, so a script can open the panel the way a
// person does. Needs Accessibility for the terminal running it.
// Usage: swift click-tray.swift [pid]
import AppKit
import ApplicationServices

let pid: pid_t
if CommandLine.arguments.count > 1, let given = Int32(CommandLine.arguments[1]) {
    pid = given
} else {
    let match = NSWorkspace.shared.runningApplications.first {
        $0.bundleIdentifier == "computer.see.app"
    }
    guard let running = match else {
        FileHandle.standardError.write("see.computer is not running\n".data(using: .utf8)!)
        exit(1)
    }
    pid = running.processIdentifier
}

func attribute(_ element: AXUIElement, _ name: String) -> AnyObject? {
    var value: AnyObject?
    return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
}

let app = AXUIElementCreateApplication(pid)
guard let bar = attribute(app, "AXExtrasMenuBar") as! AXUIElement?,
      let items = attribute(bar, kAXChildrenAttribute as String) as? [AXUIElement],
      let icon = items.first
else {
    FileHandle.standardError.write("no menu bar item found\n".data(using: .utf8)!)
    exit(1)
}

var origin = CGPoint.zero
var size = CGSize.zero
if let raw = attribute(icon, kAXPositionAttribute as String) {
    AXValueGetValue(raw as! AXValue, .cgPoint, &origin)
}
if let raw = attribute(icon, kAXSizeAttribute as String) {
    AXValueGetValue(raw as! AXValue, .cgSize, &size)
}
let center = CGPoint(x: origin.x + size.width / 2, y: origin.y + size.height / 2)

for (type, button) in [(CGEventType.leftMouseDown, 0), (.leftMouseUp, 0)] {
    let event = CGEvent(mouseEventSource: nil, mouseType: type,
                        mouseCursorPosition: center, mouseButton: CGMouseButton(rawValue: UInt32(button))!)
    event?.post(tap: .cghidEventTap)
    usleep(40_000)
}
print("clicked \(center.x), \(center.y)")
