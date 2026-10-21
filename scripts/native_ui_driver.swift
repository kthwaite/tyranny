import ApplicationServices
import Foundation

// A separate process is required: AX calls targeting the caller's own process
// execute inline and violate SwiftUI's main-actor/reentrancy requirements.
struct DriverFailure: Error, LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
func attribute(_ element: AXUIElement, _ key: String) -> CFTypeRef? {
    var result: CFTypeRef?
    AXUIElementCopyAttributeValue(element, key as CFString, &result)
    return result
}
func check(_ result: AXError, _ operation: String) throws {
    guard result == .success else { throw DriverFailure(message: "\(operation): AX error \(result.rawValue)") }
}
func find(_ identifier: String, in app: AXUIElement, byTitle: Bool = false) throws -> AXUIElement {
    func visit(_ element: AXUIElement, depth: Int) -> AXUIElement? {
        guard depth < 40 else { return nil }
        if byTitle {
            let title = attribute(element, kAXTitleAttribute) as? String
                ?? attribute(element, kAXDescriptionAttribute) as? String
            if title == identifier, attribute(element, kAXRoleAttribute) as? String == kAXButtonRole { return element }
        } else if attribute(element, kAXIdentifierAttribute) as? String == identifier {
            // NSCollectionView wraps each item's view in an AXGroup with the view's identifier
            // and no value: return the view itself.
            if attribute(element, kAXRoleAttribute) as? String == kAXGroupRole,
               attribute(element, kAXValueAttribute) == nil,
               let view = (attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []).first(where: {
                   attribute($0, kAXIdentifierAttribute) as? String == identifier
               }) { return view }
            return element
        }
        for child in attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? [] {
            if let result = visit(child, depth: depth + 1) { return result }
        }
        return nil
    }
    // SwiftUI can briefly replace an AX subtree while publishing a fresh page.
    // Wait for the actual control; never replay a mutation after an AX error.
    let deadline = ProcessInfo.processInfo.systemUptime + 3
    repeat {
        for window in attribute(app, kAXWindowsAttribute) as? [AXUIElement] ?? [] {
            // Search modal sheets before the library's potentially large row subtree.
            for sheet in attribute(window, kAXChildrenAttribute) as? [AXUIElement] ?? []
                where attribute(sheet, kAXRoleAttribute) as? String == kAXSheetRole {
                if let result = visit(sheet, depth: 0) { return result }
            }
            if let result = visit(window, depth: 0) { return result }
        }
        Thread.sleep(forTimeInterval: 0.01)
    } while ProcessInfo.processInfo.systemUptime < deadline
    let windows = attribute(app, kAXWindowsAttribute) as? [AXUIElement] ?? []
    let frontmost = attribute(app, kAXFrontmostAttribute) as? Bool
    throw DriverFailure(message: "Visible control missing: \(identifier); windows=\(windows.count); frontmost=\(String(describing: frontmost))")
}
/// A menu bar item by its titles, such as "Go›Wrapped›2026 so far". Each menu bar item and
/// submenu item holds one AXMenu, whose children are the items.
func menuItem(_ path: String, in app: AXUIElement) throws -> AXUIElement {
    guard let bar = attribute(app, kAXMenuBarAttribute), CFGetTypeID(bar) == AXUIElementGetTypeID() else {
        throw DriverFailure(message: "Menu bar unavailable")
    }
    var element = bar as! AXUIElement
    for title in path.split(separator: "›").map(String.init) {
        var items = attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? []
        if items.count == 1, attribute(items[0], kAXRoleAttribute) as? String == kAXMenuRole {
            items = attribute(items[0], kAXChildrenAttribute) as? [AXUIElement] ?? []
        }
        guard let item = items.first(where: { attribute($0, kAXTitleAttribute) as? String == title }) else {
            throw DriverFailure(message: "Menu item missing: \(path)")
        }
        element = item
    }
    return element
}

do {
    var arguments = CommandLine.arguments
    if arguments.count == 6, arguments[4] == "--value-base64" {
        guard let data = Data(base64Encoded: arguments[5]), let value = String(data: data, encoding: .utf8) else {
            throw DriverFailure(message: "Invalid encoded UTF-8 control value")
        }
        arguments.replaceSubrange(4...5, with: [value])
    }
    guard arguments.count >= 3, let pid = Int32(arguments[1]), AXIsProcessTrusted() else {
        throw DriverFailure(message: "Native UI driver requires a PID/action and Accessibility permission for its launching terminal.")
    }
    let app = AXUIElementCreateApplication(pid)
    AXUIElementSetMessagingTimeout(app, 3)
    let action = arguments[2]
    // Each invocation is a new process; regain focus if another app took it.
    try check(AXUIElementSetAttributeValue(app, kAXFrontmostAttribute as CFString, kCFBooleanTrue), "Activate application")
    if action == "activate" {
        guard let windows = attribute(app, kAXWindowsAttribute) as? [AXUIElement], !windows.isEmpty else {
            throw DriverFailure(message: "Application windows unavailable")
        }
    } else if action == "menu" || action == "expect-menu-enabled" {
        // The identifier is a menu path; "menu" presses the item without opening the menu.
        guard arguments.count >= 4 else { throw DriverFailure(message: "Menu path required") }
        let item = try menuItem(arguments[3], in: app)
        if action == "menu" {
            try check(AXUIElementPerformAction(item, kAXPressAction as CFString), "Press \(arguments[3])")
        } else {
            guard arguments.count >= 5, ["true", "false"].contains(arguments[4]),
                  let enabled = attribute(item, kAXEnabledAttribute) as? Bool,
                  enabled == (arguments[4] == "true") else {
                throw DriverFailure(message: "Unexpected enabled state for \(arguments[3])")
            }
        }
    } else {
        guard arguments.count >= 4 else { throw DriverFailure(message: "Control identifier required") }
        let control = try find(arguments[3], in: app, byTitle: action == "press-title")
        switch action {
        case "press", "press-title":
            try check(AXUIElementPerformAction(control, kAXPressAction as CFString), "Press \(arguments[3])")
        case "value":
            guard arguments.count >= 5 else { throw DriverFailure(message: "Value required") }
            try check(AXUIElementSetAttributeValue(control, kAXFocusedAttribute as CFString, kCFBooleanTrue), "Focus text field")
            try check(AXUIElementSetAttributeValue(control, kAXValueAttribute as CFString, arguments[4] as CFString), "Set value")
        case "values":
            guard arguments.count >= 5,
                  let values = try JSONSerialization.jsonObject(with: Data(arguments[4].utf8)) as? [String] else {
                throw DriverFailure(message: "Text value sequence required")
            }
            try check(AXUIElementSetAttributeValue(control, kAXFocusedAttribute as CFString, kCFBooleanTrue), "Focus text field")
            for value in values {
                try check(AXUIElementSetAttributeValue(control, kAXValueAttribute as CFString, value as CFString), "Set value")
                Thread.sleep(forTimeInterval: 0.04)
            }
        case "exists":
            break
        case "expect-enabled":
            guard arguments.count >= 5, ["true", "false"].contains(arguments[4]),
                  let enabled = attribute(control, kAXEnabledAttribute) as? Bool,
                  enabled == (arguments[4] == "true") else {
                throw DriverFailure(message: "Unexpected enabled state for \(arguments[3])")
            }
        case "slider-value":
            guard arguments.count >= 5, let value = Double(arguments[4]), value.isFinite,
                  attribute(control, kAXRoleAttribute) as? String == kAXSliderRole else {
                throw DriverFailure(message: "Slider and finite seconds value required")
            }
            try check(AXUIElementSetAttributeValue(control, kAXValueAttribute as CFString, NSNumber(value: value)), "Set slider value")
        case "increment", "decrement":
            try check(AXUIElementPerformAction(control,
                (action == "increment" ? kAXIncrementAction : kAXDecrementAction) as CFString), "Adjust slider")
        case "key":
            // "command-8" holds ⌘ with a digit: the Go menu's destination shortcuts.
            let name = arguments.count >= 5 ? arguments[4] : ""
            let command = name.hasPrefix("command-")
            let keys: [String: CGKeyCode] = ["left": 123, "right": 124, "down": 125, "up": 126,
                                             "return": 36, "escape": 53, "delete": 51, "space": 49]
            let digits: [String: CGKeyCode] = ["1": 18, "2": 19, "3": 20, "4": 21, "5": 23,
                                               "6": 22, "7": 26, "8": 28, "9": 25]
            guard let key = command ? digits[String(name.dropFirst("command-".count))] : keys[name] else {
                throw DriverFailure(message: "Arrow, return, escape, delete, space or command-digit key required")
            }
            try check(AXUIElementSetAttributeValue(control, kAXFocusedAttribute as CFString, kCFBooleanTrue), "Focus control")
            // AX focus changes are asynchronous to AppKit's key-window update.
            Thread.sleep(forTimeInterval: 0.1)
            guard let down = CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: true),
                  let up = CGEvent(keyboardEventSource: nil, virtualKey: key, keyDown: false) else {
                throw DriverFailure(message: "Cannot create keyboard events")
            }
            // Set the modifiers outright: a new event takes the current modifier state.
            down.flags = command ? .maskCommand : []
            up.flags = command ? .maskCommand : []
            down.post(tap: .cghidEventTap)
            Thread.sleep(forTimeInterval: 0.05)
            up.post(tap: .cghidEventTap)
        case "click-control", "click-row", "double-click-row":
            Thread.sleep(forTimeInterval: 0.1)
            guard action == "click-control" || arguments.count >= 5,
                  let rows = action == "click-control" ? [control] : attribute(control, kAXRowsAttribute) as? [AXUIElement] else {
                throw DriverFailure(message: "Visible control or table rows required")
            }
            let fields = (action == "click-control" ? "0,0.5" : arguments[4]).split(separator: ",")
            guard let first = fields.first, let index = Int(first), rows.indices.contains(index),
                  let position = attribute(rows[index], kAXPositionAttribute),
                  let size = attribute(rows[index], kAXSizeAttribute),
                  CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else {
                throw DriverFailure(message: "Visible row geometry required")
            }
            let fraction = fields.count > 1 ? Double(fields[1]) ?? -1 : 0.6
            var origin = CGPoint.zero
            var dimensions = CGSize.zero
            guard (0...1).contains(fraction),
                  AXValueGetValue(position as! AXValue, .cgPoint, &origin),
                  AXValueGetValue(size as! AXValue, .cgSize, &dimensions),
                  dimensions.width > 0, dimensions.height > 0 else {
                throw DriverFailure(message: "Invalid row geometry or horizontal fraction")
            }
            let point = CGPoint(x: origin.x + dimensions.width * fraction, y: origin.y + dimensions.height / 2)
            guard let move = CGEvent(mouseEventSource: nil, mouseType: .mouseMoved,
                                     mouseCursorPosition: point, mouseButton: .left) else {
                throw DriverFailure(message: "Cannot move pointer to row")
            }
            move.flags = []
            move.post(tap: .cghidEventTap)
            Thread.sleep(forTimeInterval: 0.1)
            for count in 1...(action == "double-click-row" ? 2 : 1) {
                for type in [CGEventType.leftMouseDown, .leftMouseUp] {
                    guard let event = CGEvent(mouseEventSource: nil, mouseType: type,
                                              mouseCursorPosition: point, mouseButton: .left) else {
                        throw DriverFailure(message: "Cannot create row mouse event")
                    }
                    event.setIntegerValueField(.mouseEventClickState, value: Int64(count))
                    event.flags = []
                    event.post(tap: .cghidEventTap)
                    Thread.sleep(forTimeInterval: 0.04)
                }
            }
        case "drag-rows":
            guard arguments.count >= 5,
                  let data = arguments[4].data(using: .utf8),
                  let plan = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let sourceRow = plan["row"] as? Int,
                  let steps = plan["steps"] as? [[String: Any]], !steps.isEmpty else {
                throw DriverFailure(message: "Drag requires a source row and target steps")
            }
            func point(_ element: AXUIElement, row: Int?, edge: String?) throws -> CGPoint {
                let target: AXUIElement
                if let row {
                    guard let rows = attribute(element, kAXRowsAttribute) as? [AXUIElement],
                          rows.indices.contains(row) else { throw DriverFailure(message: "Drag row is not available") }
                    target = rows[row]
                } else { target = element }
                guard let position = attribute(target, kAXPositionAttribute),
                      let size = attribute(target, kAXSizeAttribute),
                      CFGetTypeID(position) == AXValueGetTypeID(),
                      CFGetTypeID(size) == AXValueGetTypeID() else {
                    throw DriverFailure(message: "Drag target has no geometry")
                }
                var origin = CGPoint.zero
                var sizeValue = CGSize.zero
                guard AXValueGetValue(position as! AXValue, .cgPoint, &origin),
                      AXValueGetValue(size as! AXValue, .cgSize, &sizeValue),
                      sizeValue.width > 0, sizeValue.height > 0 else {
                    throw DriverFailure(message: "Invalid drag geometry")
                }
                // Aim into the inter-row gap, not inside the neighboring row.
                let y = edge == "before" ? -1.0 : edge == "after" ? sizeValue.height + 1 : sizeValue.height / 2
                return CGPoint(x: origin.x + sizeValue.width * 0.4, y: origin.y + y)
            }
            var cursor = try point(control, row: sourceRow, edge: nil)
            func mouse(_ type: CGEventType, at location: CGPoint) throws {
                guard let event = CGEvent(mouseEventSource: nil, mouseType: type,
                                          mouseCursorPosition: location, mouseButton: .left) else {
                    throw DriverFailure(message: "Cannot create drag event")
                }
                event.flags = []
                event.setIntegerValueField(.mouseEventClickState, value: 1)
                event.setDoubleValueField(.mouseEventPressure, value: type == .leftMouseUp ? 0 : 1)
                event.post(tap: .cghidEventTap)
            }
            func move(to destination: CGPoint) throws {
                let start = cursor
                for step in 1...16 {
                    let fraction = Double(step) / 16
                    cursor = CGPoint(x: start.x + (destination.x - start.x) * fraction,
                                     y: start.y + (destination.y - start.y) * fraction)
                    try mouse(.leftMouseDragged, at: cursor)
                    Thread.sleep(forTimeInterval: 0.025)
                }
            }
            Thread.sleep(forTimeInterval: 0.1)
            try mouse(.mouseMoved, at: cursor)
            try mouse(.leftMouseDown, at: cursor)
            var pressed = true
            defer { if pressed { try? mouse(.leftMouseUp, at: cursor) } }
            Thread.sleep(forTimeInterval: 0.1)
            try move(to: CGPoint(x: cursor.x + 12, y: cursor.y + 3))
            for step in steps {
                guard let identifier = step["id"] as? String else { throw DriverFailure(message: "Drag step requires target id") }
                let target = try find(identifier, in: app)
                try move(to: point(target, row: step["row"] as? Int, edge: step["edge"] as? String))
                let hold = step["hold"] as? Double ?? 0.3
                guard hold >= 0, hold <= 5 else { throw DriverFailure(message: "Drag hold must be 0...5 seconds") }
                Thread.sleep(forTimeInterval: hold)
            }
            if let window = plan["capture_window"] as? Int, let path = plan["capture_path"] as? String {
                let capture = Process()
                capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
                capture.arguments = ["-x", "-o", "-l", String(window), path]
                try capture.run()
                capture.waitUntilExit()
                guard capture.terminationStatus == 0 else { throw DriverFailure(message: "Drag screenshot failed") }
            }
            if plan["cancel"] as? Bool == true {
                guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 53, keyDown: true),
                      let up = CGEvent(keyboardEventSource: nil, virtualKey: 53, keyDown: false) else {
                    throw DriverFailure(message: "Cannot create drag cancellation key")
                }
                down.post(tap: .cghidEventTap)
                up.post(tap: .cghidEventTap)
                Thread.sleep(forTimeInterval: 0.2)
            }
            try mouse(.leftMouseUp, at: cursor)
            pressed = false
            Thread.sleep(forTimeInterval: 0.2)
        case "drag":
            guard arguments.count >= 5,
                  attribute(control, kAXRoleAttribute) as? String == kAXSliderRole,
                  let position = attribute(control, kAXPositionAttribute),
                  let size = attribute(control, kAXSizeAttribute) else {
                throw DriverFailure(message: "Visible slider geometry required")
            }
            let fractions = arguments[4].split(separator: ",").compactMap { Double($0) }
            guard fractions.count == 2, fractions.allSatisfy({ $0.isFinite && (0...1).contains($0) }),
                  CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else {
                throw DriverFailure(message: "Drag requires start,end fractions in 0...1")
            }
            var origin = CGPoint.zero
            var dimensions = CGSize.zero
            guard AXValueGetValue(position as! AXValue, .cgPoint, &origin),
                  AXValueGetValue(size as! AXValue, .cgSize, &dimensions), dimensions.width > 20 else {
                throw DriverFailure(message: "Invalid slider geometry")
            }
            func mouse(_ type: CGEventType, fraction: Double) throws {
                let point = CGPoint(x: origin.x + 8 + (dimensions.width - 16) * fraction,
                                    y: origin.y + dimensions.height / 2)
                guard let event = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: .left) else {
                    throw DriverFailure(message: "Cannot create mouse event")
                }
                event.post(tap: .cghidEventTap)
            }
            try mouse(.leftMouseDown, fraction: fractions[0])
            for step in 1...10 {
                Thread.sleep(forTimeInterval: 0.05)
                try mouse(.leftMouseDragged, fraction: fractions[0] + (fractions[1] - fractions[0]) * Double(step) / 10)
            }
            try mouse(.leftMouseUp, fraction: fractions[1])
        case "scroll-fraction":
            // Moves the enclosing scroll area's vertical scroller, as VoiceOver does.
            guard arguments.count >= 5, let fraction = Double(arguments[4]), (0...1).contains(fraction) else {
                throw DriverFailure(message: "Scroll requires a fraction in 0...1")
            }
            var area: AXUIElement? = control
            while let element = area, attribute(element, kAXRoleAttribute) as? String != kAXScrollAreaRole {
                guard let parent = attribute(element, kAXParentAttribute) else { area = nil; break }
                area = (parent as! AXUIElement)
            }
            guard let area, let bar = attribute(area, kAXVerticalScrollBarAttribute) else {
                throw DriverFailure(message: "No vertical scroll bar for \(arguments[3])")
            }
            try check(AXUIElementSetAttributeValue(bar as! AXUIElement, kAXValueAttribute as CFString,
                                                   NSNumber(value: fraction)), "Set scroll position")
        case "expect-text":
            guard arguments.count >= 5 else { throw DriverFailure(message: "Expected text required") }
            let text = attribute(control, kAXValueAttribute) as? String
                ?? attribute(control, kAXTitleAttribute) as? String
                ?? attribute(control, kAXDescriptionAttribute) as? String
            guard text == arguments[4] else {
                throw DriverFailure(message: "Visible text mismatch for \(arguments[3]): \(String(describing: text))")
            }
        case "choose":
            // A pop-up button (SwiftUI menu Picker): open it, then press the item with the title.
            guard arguments.count >= 5 else { throw DriverFailure(message: "Item title required") }
            try check(AXUIElementPerformAction(control, kAXPressAction as CFString), "Open \(arguments[3])")
            func item(_ element: AXUIElement, depth: Int) -> AXUIElement? {
                guard depth < 8 else { return nil }
                if attribute(element, kAXRoleAttribute) as? String == kAXMenuItemRole,
                   attribute(element, kAXTitleAttribute) as? String == arguments[4] { return element }
                for child in attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? [] {
                    if let found = item(child, depth: depth + 1) { return found }
                }
                return nil
            }
            let deadline = ProcessInfo.processInfo.systemUptime + 3
            var found: AXUIElement?
            repeat {
                found = item(control, depth: 0)
                if found == nil { Thread.sleep(forTimeInterval: 0.05) }
            } while found == nil && ProcessInfo.processInfo.systemUptime < deadline
            guard let found else { throw DriverFailure(message: "Menu item missing: \(arguments[4])") }
            try check(AXUIElementPerformAction(found, kAXPressAction as CFString), "Choose \(arguments[4])")
            // Choosing the already-selected item need not change model state.
            // Wait for AppKit to dismiss its menu before another invocation can
            // press the picker; otherwise that press closes the old menu.
            func menuOpen(_ element: AXUIElement, depth: Int) -> Bool {
                guard depth < 8 else { return false }
                if attribute(element, kAXRoleAttribute) as? String == kAXMenuRole { return true }
                return (attribute(element, kAXChildrenAttribute) as? [AXUIElement] ?? [])
                    .contains { menuOpen($0, depth: depth + 1) }
            }
            let dismissalDeadline = ProcessInfo.processInfo.systemUptime + 3
            while menuOpen(try find(arguments[3], in: app), depth: 0) {
                guard ProcessInfo.processInfo.systemUptime < dismissalDeadline else {
                    throw DriverFailure(message: "Menu did not close after choosing \(arguments[4])")
                }
                Thread.sleep(forTimeInterval: 0.01)
            }
        case "expect-row-count":
            guard arguments.count >= 5, let expected = Int(arguments[4]) else {
                throw DriverFailure(message: "Row count required")
            }
            var count: CFIndex = 0
            try check(AXUIElementGetAttributeValueCount(control, kAXRowsAttribute as CFString, &count), "Count rows")
            guard count == expected else {
                throw DriverFailure(message: "Row count mismatch for \(arguments[3]): \(count), expected \(expected)")
            }
        case "select":
            guard arguments.count >= 5,
                  let rows = attribute(control, kAXRowsAttribute) as? [AXUIElement] else { throw DriverFailure(message: "Selectable rows missing") }
            let indexes = arguments[4].split(separator: ",").compactMap { Int($0) }
            guard !indexes.isEmpty, indexes.allSatisfy({ $0 >= 0 && $0 < rows.count }) else { throw DriverFailure(message: "Selection outside visible rows") }
            try check(AXUIElementSetAttributeValue(control, kAXSelectedRowsAttribute as CFString, indexes.map { rows[$0] } as CFArray), "Select rows")
        default:
            throw DriverFailure(message: "Unknown native UI action")
        }
    }
} catch {
    FileHandle.standardError.write(Data((error.localizedDescription + "\n").utf8))
    exit(1)
}
