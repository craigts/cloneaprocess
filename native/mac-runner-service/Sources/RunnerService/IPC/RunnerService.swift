import Foundation
#if canImport(ApplicationServices)
import ApplicationServices
#endif
#if canImport(AppKit)
import AppKit
#endif
#if canImport(ImageIO)
import ImageIO
#endif
#if canImport(UniformTypeIdentifiers)
import UniformTypeIdentifiers
#endif

private let runnerProtocolVersion = 1
private let runnerProtocolMinimumVersion = 1
private let runnerProtocolCapabilities = [
    "run_workflow",
    "abort_run",
    "event_stream",
    "ax_actions",
    "focus_window",
    "set_text",
    "menu_navigation",
    "verification_hooks",
    "key_press",
    "screenshot",
    "subprocess_bridge",
    "computer_use",
]

protocol RunnerActionPerforming {
    func focusWindow(bundleId: String, title: String?) throws -> [String: Any]
    func click(selector: [String: Any]) throws -> [String: Any]
    func rightClick(selector: [String: Any]) throws -> [String: Any]
    func clickAt(x: Double, y: Double, button: String) throws -> [String: Any]
    // Computer-use clicking: multi-click (double/triple) and held modifiers (shift/cmd-click).
    func clickAt(x: Double, y: Double, button: String, clickCount: Int, modifiers: [String]) throws -> [String: Any]
    func setText(selector: [String: Any], value: String) throws -> [String: Any]
    func setTextFocused(value: String) throws -> [String: Any]
    // Types a literal string as real keystrokes into the focused element (reliable in browsers,
    // unlike AX value-set). Backs the computer-use `type` action.
    func typeText(text: String) throws -> [String: Any]
    func keyPress(key: String, modifiers: [String]) throws -> [String: Any]
    func moveMouse(x: Double, y: Double) throws -> [String: Any]
    func scroll(x: Double, y: Double, direction: String, amount: Int, modifiers: [String]) throws -> [String: Any]
    func dragTo(fromX: Double, fromY: Double, toX: Double, toY: Double) throws -> [String: Any]
    func selectMenu(path: [String]) throws -> [String: Any]
    func waitForCondition(condition: [String: Any], timeoutMs: UInt64) throws -> [String: Any]
    func assertCondition(condition: [String: Any]) throws -> [String: Any]
    func takeScreenshot(quality: Double) throws -> [String: Any]
}

// Default implementations so conformers (e.g. test mocks) needn't implement every computer-use
// primitive. Declaring them in the protocol body keeps dispatch dynamic — `LiveRunnerActionPerformer`
// overrides take effect when called through the protocol.
extension RunnerActionPerforming {
    func clickAt(x: Double, y: Double, button: String, clickCount: Int, modifiers: [String]) throws -> [String: Any] {
        try clickAt(x: x, y: y, button: button)
    }
    func typeText(text: String) throws -> [String: Any] {
        throw RunnerServiceError.unsupportedStep("typeText is not supported by this performer")
    }
    func moveMouse(x: Double, y: Double) throws -> [String: Any] {
        throw RunnerServiceError.unsupportedStep("moveMouse is not supported by this performer")
    }
    func scroll(x: Double, y: Double, direction: String, amount: Int, modifiers: [String]) throws -> [String: Any] {
        throw RunnerServiceError.unsupportedStep("scroll is not supported by this performer")
    }
    func dragTo(fromX: Double, fromY: Double, toX: Double, toY: Double) throws -> [String: Any] {
        throw RunnerServiceError.unsupportedStep("dragTo is not supported by this performer")
    }
}

enum RunnerServiceError: Error {
    case invalidRequest(String)
    case unsupportedStep(String)
    case executionFailed(String)

    var code: String {
        switch self {
        case .invalidRequest:
            return "INVALID_REQUEST"
        case .unsupportedStep:
            return "UNSUPPORTED_STEP"
        case .executionFailed:
            return "EXECUTION_FAILED"
        }
    }

    var message: String {
        switch self {
        case .invalidRequest(let message), .unsupportedStep(let message), .executionFailed(let message):
            return message
        }
    }
}

final class RunnerBridgeSession {
    private let performer: RunnerActionPerforming

    init(performer: RunnerActionPerforming) {
        self.performer = performer
    }

    func handle(jsonLine: String) -> [String] {
        guard let data = jsonLine.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else {
            return [serializeReply(id: "invalid", ok: false, error: .invalidRequest("request must be a JSON object"))]
        }

        let requestId = object["id"] as? String ?? "req_invalid"
        let requestType = object["type"] as? String ?? ""
        let payload = object["payload"] as? [String: Any] ?? [:]

        do {
            switch requestType {
            case "ping":
                return [
                    serializeReply(
                        id: requestId,
                        ok: true,
                        payload: protocolHandshakePayload()
                    ),
                ]
            case "run_workflow":
                return try handleRunWorkflow(requestId: requestId, payload: payload)
            case "take_screenshot":
                let quality = (payload["quality"] as? NSNumber)?.doubleValue ?? 0.6
                let result = try performer.takeScreenshot(quality: quality)
                return [serializeReply(id: requestId, ok: true, payload: result)]
            case "abort_run":
                return [
                    serializeReply(
                        id: requestId,
                        ok: false,
                        error: .executionFailed("no active run to abort")
                    ),
                ]
            default:
                return [
                    serializeReply(
                        id: requestId,
                        ok: false,
                        error: .invalidRequest("unsupported request type \(requestType)")
                    ),
                ]
            }
        } catch let error as RunnerServiceError {
            return [serializeReply(id: requestId, ok: false, error: error)]
        } catch {
            return [serializeReply(id: requestId, ok: false, error: .executionFailed(error.localizedDescription))]
        }
    }

    private func handleRunWorkflow(requestId: String, payload: [String: Any]) throws -> [String] {
        let workflowId = payload["workflow_id"] as? String ?? "wf_unknown"
        let runId = payload["run_id"] as? String ?? "run_\(UUID().uuidString.lowercased())"
        guard let steps = payload["steps"] as? [[String: Any]] else {
            throw RunnerServiceError.invalidRequest("run_workflow payload requires a steps array")
        }

        let startedAt = nowMs()
        var lines = [
            serializeReply(id: requestId, ok: true, payload: ["run_id": runId]),
            serializeEvent(type: "run_started", payload: ["run_id": runId, "workflow_id": workflowId]),
        ]

        for (index, step) in steps.enumerated() {
            let kind = step["kind"] as? String ?? "unknown"
            lines.append(serializeEvent(type: "step_started", payload: [
                "run_id": runId,
                "step_index": index,
                "kind": kind,
            ]))

            do {
                let result = try perform(step: step)
                lines.append(serializeEvent(type: "step_finished", payload: [
                    "run_id": runId,
                    "step_index": index,
                    "ok": true,
                    "result": result,
                ]))
            } catch let error as RunnerServiceError {
                let errorPayload = [
                    "code": error.code,
                    "message": error.message,
                    "retryable": false,
                ] as [String : Any]
                lines.append(serializeEvent(type: "step_finished", payload: [
                    "run_id": runId,
                    "step_index": index,
                    "ok": false,
                    "error": errorPayload,
                ]))
                lines.append(serializeEvent(type: "run_failed", payload: [
                    "run_id": runId,
                    "error": errorPayload,
                ]))
                return lines
            }
        }

        lines.append(serializeEvent(type: "run_completed", payload: [
            "run_id": runId,
            "ok": true,
            "duration_ms": nowMs() - startedAt,
        ]))
        return lines
    }

    private func perform(step: [String: Any]) throws -> [String: Any] {
        guard let kind = step["kind"] as? String else {
            throw RunnerServiceError.invalidRequest("step kind is required")
        }

        switch kind {
        case "focusWindow":
            guard let bundleId = step["bundleId"] as? String, !bundleId.isEmpty else {
                throw RunnerServiceError.invalidRequest("focusWindow step requires bundleId")
            }
            return try performer.focusWindow(bundleId: bundleId, title: step["title"] as? String)
        case "click":
            guard let selector = step["selector"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("click step requires selector")
            }
            return try performer.click(selector: selector)
        case "setText":
            let selector = step["selector"] as? [String: Any]
            let literalValue: String
            if let value = step["value"] as? [String: Any] {
                let kind = value["kind"] as? String ?? ""
                if kind == "literal", let v = value["value"] as? String {
                    literalValue = v
                } else if let v = value["default"] as? String {
                    literalValue = v
                } else if let v = value["value"] as? String {
                    literalValue = v
                } else {
                    throw RunnerServiceError.invalidRequest("setText step requires a literal value")
                }
            } else if let plainValue = step["value"] as? String {
                literalValue = plainValue
            } else {
                throw RunnerServiceError.invalidRequest("setText step requires a value")
            }
            if let selector {
                return try performer.setText(selector: selector, value: literalValue)
            } else {
                return try performer.setTextFocused(value: literalValue)
            }
        case "rightClick":
            guard let selector = step["selector"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("rightClick step requires selector")
            }
            return try performer.rightClick(selector: selector)
        case "clickAt":
            guard let x = (step["x"] as? NSNumber)?.doubleValue,
                  let y = (step["y"] as? NSNumber)?.doubleValue else {
                throw RunnerServiceError.invalidRequest("clickAt step requires x and y coordinates")
            }
            let button = step["button"] as? String ?? "left"
            let clickCount = (step["clickCount"] as? NSNumber)?.intValue ?? 1
            let modifiers = step["modifiers"] as? [String] ?? []
            return try performer.clickAt(x: x, y: y, button: button, clickCount: clickCount, modifiers: modifiers)
        case "rightClickAt":
            guard let x = (step["x"] as? NSNumber)?.doubleValue,
                  let y = (step["y"] as? NSNumber)?.doubleValue else {
                throw RunnerServiceError.invalidRequest("rightClickAt step requires x and y coordinates")
            }
            return try performer.clickAt(x: x, y: y, button: "right", clickCount: 1, modifiers: [])
        case "moveMouse":
            guard let x = (step["x"] as? NSNumber)?.doubleValue,
                  let y = (step["y"] as? NSNumber)?.doubleValue else {
                throw RunnerServiceError.invalidRequest("moveMouse step requires x and y coordinates")
            }
            return try performer.moveMouse(x: x, y: y)
        case "typeText":
            guard let text = step["text"] as? String else {
                throw RunnerServiceError.invalidRequest("typeText step requires text")
            }
            return try performer.typeText(text: text)
        case "scroll":
            guard let x = (step["x"] as? NSNumber)?.doubleValue,
                  let y = (step["y"] as? NSNumber)?.doubleValue else {
                throw RunnerServiceError.invalidRequest("scroll step requires x and y coordinates")
            }
            let direction = step["direction"] as? String ?? "down"
            let amount = (step["amount"] as? NSNumber)?.intValue ?? 3
            let modifiers = step["modifiers"] as? [String] ?? []
            return try performer.scroll(x: x, y: y, direction: direction, amount: amount, modifiers: modifiers)
        case "drag":
            guard let fromX = (step["fromX"] as? NSNumber)?.doubleValue,
                  let fromY = (step["fromY"] as? NSNumber)?.doubleValue,
                  let toX = (step["toX"] as? NSNumber)?.doubleValue,
                  let toY = (step["toY"] as? NSNumber)?.doubleValue else {
                throw RunnerServiceError.invalidRequest("drag step requires fromX, fromY, toX, toY")
            }
            return try performer.dragTo(fromX: fromX, fromY: fromY, toX: toX, toY: toY)
        case "keyPress":
            guard let key = step["key"] as? String, !key.isEmpty else {
                throw RunnerServiceError.invalidRequest("keyPress step requires key")
            }
            let modifiers = step["modifiers"] as? [String] ?? []
            return try performer.keyPress(key: key, modifiers: modifiers)
        case "delay":
            let ms = (step["ms"] as? NSNumber)?.intValue ?? 1000
            Thread.sleep(forTimeInterval: Double(ms) / 1000.0)
            return ["action": "delay", "ms": ms]
        case "selectMenu":
            guard let path = step["path"] as? [String], !path.isEmpty else {
                throw RunnerServiceError.invalidRequest("selectMenu step requires a non-empty path")
            }
            return try performer.selectMenu(path: path)
        case "waitFor":
            guard let condition = step["condition"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("waitFor step requires condition")
            }
            let timeoutMs = (step["timeoutMs"] as? NSNumber)?.uint64Value ?? 1_500
            return try performer.waitForCondition(condition: condition, timeoutMs: timeoutMs)
        case "assert":
            guard let condition = step["condition"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("assert step requires condition")
            }
            return try performer.assertCondition(condition: condition)
        default:
            throw RunnerServiceError.unsupportedStep("unsupported step kind \(kind)")
        }
    }

    private func serializeReply(id: String, ok: Bool, payload: [String: Any] = [:], error: RunnerServiceError? = nil) -> String {
        var reply: [String: Any] = [
            "v": 1,
            "id": id,
            "ok": ok,
            "payload": payload,
        ]

        if let error {
            reply["error"] = [
                "code": error.code,
                "message": error.message,
                "retryable": false,
            ]
        } else {
            reply["error"] = NSNull()
        }

        return serializeJSONObject(reply)
    }

    private func serializeEvent(type: String, payload: [String: Any]) -> String {
        serializeJSONObject([
            "v": 1,
            "id": "evt_\(UUID().uuidString.lowercased())",
            "ts": nowMs(),
            "type": type,
            "payload": payload,
        ])
    }

    private func serializeJSONObject(_ object: [String: Any]) -> String {
        let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        return String(data: data ?? Data("{}".utf8), encoding: .utf8) ?? "{}"
    }

    private func nowMs() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }
}

func protocolHandshakePayload() -> [String: Any] {
    [
        "protocol_version": runnerProtocolVersion,
        "protocol_min": runnerProtocolMinimumVersion,
        "capabilities": runnerProtocolCapabilities,
    ]
}

struct RunnerService {
    private let session: RunnerBridgeSession

    init(session: RunnerBridgeSession = RunnerBridgeSession(performer: LiveRunnerActionPerformer())) {
        self.session = session
    }

    func runBridge() {
        // Use raw POSIX I/O to avoid all Foundation and C stdio buffering issues
        // when spawned as a subprocess from the Tauri app.
        var buffer = Data()
        let readBuf = UnsafeMutablePointer<UInt8>.allocate(capacity: 4096)
        defer { readBuf.deallocate() }

        while true {
            let bytesRead = read(STDIN_FILENO, readBuf, 4096)
            guard bytesRead > 0 else { break }
            buffer.append(readBuf, count: bytesRead)

            while let newlineIndex = buffer.firstIndex(of: UInt8(ascii: "\n")) {
                let lineData = buffer[buffer.startIndex..<newlineIndex]
                buffer = buffer[(newlineIndex + 1)...]
                guard let rawLine = String(data: lineData, encoding: .utf8), !rawLine.isEmpty else { continue }
                let lines = session.handle(jsonLine: rawLine)
                for line in lines {
                    let outputLine = line + "\n"
                    if let data = outputLine.data(using: .utf8) {
                        data.withUnsafeBytes { rawBuf in
                            var remaining = rawBuf.count
                            var offset = 0
                            while remaining > 0 {
                                let written = Darwin.write(STDOUT_FILENO, rawBuf.baseAddress! + offset, remaining)
                                if written <= 0 { return }
                                offset += written
                                remaining -= written
                            }
                        }
                    }
                }
            }
        }
    }

    func run() {
        print("RunnerService bridge available via --bridge.")
    }
}

struct LiveRunnerActionPerformer: RunnerActionPerforming {
    func focusWindow(bundleId: String, title: String?) throws -> [String: Any] {
        #if canImport(AppKit)
        guard let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first else {
            throw RunnerServiceError.executionFailed("application \(bundleId) is not running")
        }
        let activated = app.activate()
        guard activated else {
            throw RunnerServiceError.executionFailed("failed to activate application \(bundleId)")
        }
        var result: [String: Any] = [
            "action": "focusWindow",
            "bundleId": bundleId,
        ]
        if let title {
            result["title"] = title
        }
        return result
        #else
        _ = title
        throw RunnerServiceError.executionFailed("window focus is only available on macOS")
        #endif
    }

    func click(selector: [String: Any]) throws -> [String: Any] {
        let element = try resolveElement(selector: selector)
        let result = AXUIElementPerformAction(element, kAXPressAction as CFString)
        guard result == .success else {
            throw RunnerServiceError.executionFailed("AXPress failed with code \(result.rawValue)")
        }
        return ["action": "click"]
    }

    func rightClick(selector: [String: Any]) throws -> [String: Any] {
        let element = try resolveElement(selector: selector)
        let result = AXUIElementPerformAction(element, kAXShowMenuAction as CFString)
        guard result == .success else {
            throw RunnerServiceError.executionFailed("AXShowMenu failed with code \(result.rawValue)")
        }
        return ["action": "rightClick"]
    }

    func setText(selector: [String: Any], value: String) throws -> [String: Any] {
        let element = try resolveElement(selector: selector)
        let result = AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFTypeRef)
        guard result == .success else {
            throw RunnerServiceError.executionFailed("set value failed with code \(result.rawValue)")
        }
        return ["action": "setText", "value": value]
    }

    func clickAt(x: Double, y: Double, button: String) throws -> [String: Any] {
        try clickAt(x: x, y: y, button: button, clickCount: 1, modifiers: [])
    }

    func clickAt(x: Double, y: Double, button: String, clickCount: Int, modifiers: [String]) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        let point = CGPoint(x: x, y: y)
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw RunnerServiceError.executionFailed("failed to create event source")
        }

        let (mouseButton, downType, upType): (CGMouseButton, CGEventType, CGEventType)
        switch button {
        case "right":
            (mouseButton, downType, upType) = (.right, .rightMouseDown, .rightMouseUp)
        case "middle", "center", "other":
            (mouseButton, downType, upType) = (.center, .otherMouseDown, .otherMouseUp)
        default:
            (mouseButton, downType, upType) = (.left, .leftMouseDown, .leftMouseUp)
        }

        let flags = try Self.eventFlags(for: modifiers)
        let count = max(1, clickCount)

        // A genuine double/triple click is a sequence of down/up pairs with an incrementing
        // click-state field — posting independent single clicks won't register as a double click.
        for click in 1...count {
            guard let mouseDown = CGEvent(mouseEventSource: source, mouseType: downType, mouseCursorPosition: point, mouseButton: mouseButton),
                  let mouseUp = CGEvent(mouseEventSource: source, mouseType: upType, mouseCursorPosition: point, mouseButton: mouseButton) else {
                throw RunnerServiceError.executionFailed("failed to create mouse event")
            }
            mouseDown.setIntegerValueField(.mouseEventClickState, value: Int64(click))
            mouseUp.setIntegerValueField(.mouseEventClickState, value: Int64(click))
            mouseDown.flags = flags
            mouseUp.flags = flags
            mouseDown.post(tap: .cghidEventTap)
            Thread.sleep(forTimeInterval: 0.04)
            mouseUp.post(tap: .cghidEventTap)
            if click < count { Thread.sleep(forTimeInterval: 0.04) }
        }

        return ["action": "clickAt", "x": x, "y": y, "button": button, "clickCount": count, "modifiers": modifiers]
        #else
        throw RunnerServiceError.executionFailed("clickAt is only available on macOS")
        #endif
    }

    func moveMouse(x: Double, y: Double) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        guard let source = CGEventSource(stateID: .hidSystemState),
              let move = CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left) else {
            throw RunnerServiceError.executionFailed("failed to create mouse move event")
        }
        move.post(tap: .cghidEventTap)
        return ["action": "moveMouse", "x": x, "y": y]
        #else
        throw RunnerServiceError.executionFailed("moveMouse is only available on macOS")
        #endif
    }

    func typeText(text: String) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw RunnerServiceError.executionFailed("failed to create event source")
        }
        // Drive real keystrokes via a synthesized Unicode key event. Unlike AX value-set, this
        // reaches web inputs and any focused control. Chunk to stay within the per-event buffer.
        let scalars = Array(text.utf16)
        let chunkSize = 20
        var index = 0
        while index < scalars.count {
            let chunk = Array(scalars[index..<min(index + chunkSize, scalars.count)])
            guard let keyDown = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                  let keyUp = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else {
                throw RunnerServiceError.executionFailed("failed to create keyboard event")
            }
            keyDown.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: chunk)
            keyUp.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: chunk)
            keyDown.post(tap: .cghidEventTap)
            keyUp.post(tap: .cghidEventTap)
            Thread.sleep(forTimeInterval: 0.01)
            index += chunkSize
        }
        return ["action": "typeText", "length": scalars.count]
        #else
        throw RunnerServiceError.executionFailed("typeText is only available on macOS")
        #endif
    }

    func scroll(x: Double, y: Double, direction: String, amount: Int, modifiers: [String]) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw RunnerServiceError.executionFailed("failed to create event source")
        }
        // Position the cursor over the target first so the scroll lands in the right view.
        if let move = CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left) {
            move.post(tap: .cghidEventTap)
        }

        // Each "amount" unit is a few scroll lines. Vertical → wheel1, horizontal → wheel2.
        let lines = Int32(max(1, amount) * 3)
        var wheel1: Int32 = 0
        var wheel2: Int32 = 0
        switch direction.lowercased() {
        case "up": wheel1 = lines
        case "down": wheel1 = -lines
        case "left": wheel2 = lines
        case "right": wheel2 = -lines
        default: throw RunnerServiceError.invalidRequest("unknown scroll direction: \(direction)")
        }

        guard let scrollEvent = CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2, wheel1: wheel1, wheel2: wheel2, wheel3: 0) else {
            throw RunnerServiceError.executionFailed("failed to create scroll event")
        }
        scrollEvent.flags = try Self.eventFlags(for: modifiers)
        scrollEvent.post(tap: .cghidEventTap)
        return ["action": "scroll", "direction": direction, "amount": amount]
        #else
        throw RunnerServiceError.executionFailed("scroll is only available on macOS")
        #endif
    }

    func dragTo(fromX: Double, fromY: Double, toX: Double, toY: Double) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw RunnerServiceError.executionFailed("failed to create event source")
        }
        let start = CGPoint(x: fromX, y: fromY)
        let end = CGPoint(x: toX, y: toY)
        guard let down = CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: start, mouseButton: .left),
              let drag = CGEvent(mouseEventSource: source, mouseType: .leftMouseDragged, mouseCursorPosition: end, mouseButton: .left),
              let up = CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: end, mouseButton: .left) else {
            throw RunnerServiceError.executionFailed("failed to create drag event")
        }
        down.post(tap: .cghidEventTap)
        Thread.sleep(forTimeInterval: 0.05)
        drag.post(tap: .cghidEventTap)
        Thread.sleep(forTimeInterval: 0.05)
        up.post(tap: .cghidEventTap)
        return ["action": "dragTo", "fromX": fromX, "fromY": fromY, "toX": toX, "toY": toY]
        #else
        throw RunnerServiceError.executionFailed("dragTo is only available on macOS")
        #endif
    }

    #if canImport(ApplicationServices)
    /// Maps modifier names (cmd/shift/alt/ctrl, plus the computer-use `super` alias for Command)
    /// to CGEventFlags. Shared by click and scroll.
    static func eventFlags(for modifiers: [String]) throws -> CGEventFlags {
        var flags: CGEventFlags = []
        for mod in modifiers {
            switch mod.lowercased() {
            case "cmd", "command", "super", "meta": flags.insert(.maskCommand)
            case "shift": flags.insert(.maskShift)
            case "alt", "option": flags.insert(.maskAlternate)
            case "ctrl", "control": flags.insert(.maskControl)
            default: throw RunnerServiceError.invalidRequest("unknown modifier: \(mod)")
            }
        }
        return flags
    }
    #endif

    func setTextFocused(value: String) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        guard AXIsProcessTrusted() else {
            throw RunnerServiceError.executionFailed("Accessibility permission is required")
        }
        let systemWide = AXUIElementCreateSystemWide()
        var focusedRaw: CFTypeRef?
        let result = AXUIElementCopyAttributeValue(systemWide, kAXFocusedUIElementAttribute as CFString, &focusedRaw)
        guard result == .success, let focused = focusedRaw else {
            throw RunnerServiceError.executionFailed("no focused element found")
        }
        let element = focused as! AXUIElement
        let setResult = AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFTypeRef)
        guard setResult == .success else {
            throw RunnerServiceError.executionFailed("set value on focused element failed with code \(setResult.rawValue)")
        }
        return ["action": "setText", "value": value, "target": "focused"]
        #else
        throw RunnerServiceError.executionFailed("setText is only available on macOS")
        #endif
    }

    func keyPress(key: String, modifiers: [String]) throws -> [String: Any] {
        #if canImport(ApplicationServices)
        guard let keyCode = keyCodeForName(key) else {
            throw RunnerServiceError.invalidRequest("unknown key: \(key)")
        }

        let flags = try Self.eventFlags(for: modifiers)

        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw RunnerServiceError.executionFailed("failed to create event source")
        }

        guard let keyDown = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: true) else {
            throw RunnerServiceError.executionFailed("failed to create key down event")
        }
        keyDown.flags = flags

        guard let keyUp = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: false) else {
            throw RunnerServiceError.executionFailed("failed to create key up event")
        }
        keyUp.flags = flags

        keyDown.post(tap: .cghidEventTap)
        keyUp.post(tap: .cghidEventTap)

        return ["action": "keyPress", "key": key, "modifiers": modifiers]
        #else
        throw RunnerServiceError.executionFailed("keyPress is only available on macOS")
        #endif
    }

    func selectMenu(path: [String]) throws -> [String: Any] {
        #if canImport(AppKit)
        guard AXIsProcessTrusted() else {
            throw RunnerServiceError.executionFailed("Accessibility permission is required")
        }
        guard let app = NSWorkspace.shared.frontmostApplication else {
            throw RunnerServiceError.executionFailed("frontmost application is unavailable")
        }
        let appElement = AXUIElementCreateApplication(app.processIdentifier)
        guard let menuBar = copyAttribute(element: appElement, attribute: kAXMenuBarAttribute as CFString) as! AXUIElement? else {
            throw RunnerServiceError.executionFailed("menu bar is unavailable")
        }

        var currentElement = menuBar
        for title in path {
            guard let next = findChild(element: currentElement, title: title) else {
                throw RunnerServiceError.executionFailed("menu item '\(title)' not found")
            }
            currentElement = next
            let result = AXUIElementPerformAction(currentElement, kAXPressAction as CFString)
            guard result == .success else {
                throw RunnerServiceError.executionFailed("menu action failed for '\(title)'")
            }
        }

        return ["action": "selectMenu", "path": path]
        #else
        throw RunnerServiceError.executionFailed("menu traversal is only available on macOS")
        #endif
    }

    func waitForCondition(condition: [String: Any], timeoutMs: UInt64) throws -> [String: Any] {
        let deadline = Date().timeIntervalSince1970 + (Double(timeoutMs) / 1000.0)
        var lastFailure = "condition did not match"

        repeat {
            do {
                var result = try assertCondition(condition: condition)
                result["action"] = "waitFor"
                result["timeoutMs"] = timeoutMs
                return result
            } catch let error as RunnerServiceError {
                switch error {
                case .executionFailed(let message):
                    lastFailure = message
                    Thread.sleep(forTimeInterval: 0.05)
                case .invalidRequest, .unsupportedStep:
                    throw error
                }
            }
        } while Date().timeIntervalSince1970 < deadline

        throw RunnerServiceError.executionFailed(
            "timed out after \(timeoutMs)ms waiting for condition: \(lastFailure)"
        )
    }

    func assertCondition(condition: [String: Any]) throws -> [String: Any] {
        guard let kind = condition["kind"] as? String else {
            throw RunnerServiceError.invalidRequest("condition kind is required")
        }

        switch kind {
        case "elementPresent":
            guard let selector = condition["selector"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("elementPresent condition requires selector")
            }
            let element = try resolveElement(selector: selector)
            return [
                "action": "assert",
                "conditionKind": kind,
                "matched": describeElement(element),
            ]
        case "textEquals":
            guard let selector = condition["selector"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("textEquals condition requires selector")
            }
            guard let expectedValue = condition["value"] as? String else {
                throw RunnerServiceError.invalidRequest("textEquals condition requires value")
            }
            let element = try resolveElement(selector: selector)
            let actualValue = attributeString(kAXValueAttribute as CFString, element: element) ?? ""
            guard actualValue == expectedValue else {
                throw RunnerServiceError.executionFailed(
                    "expected value '\(expectedValue)' but found '\(actualValue)'"
                )
            }
            return [
                "action": "assert",
                "conditionKind": kind,
                "expectedValue": expectedValue,
                "actualValue": actualValue,
                "matched": describeElement(element),
            ]
        case "windowVisible":
            return try assertWindowVisible(condition: condition)
        default:
            throw RunnerServiceError.invalidRequest("unsupported condition kind \(kind)")
        }
    }

    private func resolveElement(selector: [String: Any]) throws -> AXUIElement {
        guard AXIsProcessTrusted() else {
            throw RunnerServiceError.executionFailed("Accessibility permission is required")
        }

        let appElement = try resolveApplicationElement(selector: selector)
        let criteria = SelectorCriteria(selector: selector)
        guard let element = findMatchingElement(root: appElement, criteria: criteria, depth: 0, visited: 0).element else {
            throw RunnerServiceError.executionFailed("selector did not match an accessibility element")
        }
        return element
    }

    private func resolveApplicationElement(selector: [String: Any]) throws -> AXUIElement {
        #if canImport(AppKit)
        if let bundleId = selectorTargetBundleId(selector),
           let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first
        {
            return AXUIElementCreateApplication(app.processIdentifier)
        }

        guard let frontmost = NSWorkspace.shared.frontmostApplication else {
            throw RunnerServiceError.executionFailed("frontmost application is unavailable")
        }
        return AXUIElementCreateApplication(frontmost.processIdentifier)
        #else
        throw RunnerServiceError.executionFailed("application lookup is only available on macOS")
        #endif
    }

    private func selectorTargetBundleId(_ selector: [String: Any]) -> String? {
        if let targetApp = selector["target_app"] as? [String: Any],
           let bundleId = targetApp["bundle_id"] as? String,
           !bundleId.isEmpty
        {
            return bundleId
        }

        if let targetApp = selector["targetApp"] as? [String: Any],
           let bundleId = targetApp["bundleId"] as? String,
           !bundleId.isEmpty
        {
            return bundleId
        }

        return nil
    }

    private func assertWindowVisible(condition: [String: Any]) throws -> [String: Any] {
        #if canImport(AppKit)
        guard let bundleId = condition["bundleId"] as? String, !bundleId.isEmpty else {
            throw RunnerServiceError.invalidRequest("windowVisible condition requires bundleId")
        }
        guard let frontmost = NSWorkspace.shared.frontmostApplication else {
            throw RunnerServiceError.executionFailed("frontmost application is unavailable")
        }
        let frontmostBundleId = frontmost.bundleIdentifier ?? "unknown"
        guard frontmostBundleId == bundleId else {
            throw RunnerServiceError.executionFailed(
                "expected frontmost app \(bundleId) but found \(frontmostBundleId)"
            )
        }

        let expectedTitle = condition["title"] as? String
        let actualTitle = try focusedWindowTitle(for: frontmost)
        if let expectedTitle, actualTitle != expectedTitle {
            throw RunnerServiceError.executionFailed(
                "expected focused window title '\(expectedTitle)' but found '\(actualTitle ?? "")'"
            )
        }

        var result: [String: Any] = [
            "action": "assert",
            "conditionKind": "windowVisible",
            "bundleId": bundleId,
        ]
        if let actualTitle {
            result["actualTitle"] = actualTitle
        }
        return result
        #else
        throw RunnerServiceError.executionFailed("window verification is only available on macOS")
        #endif
    }

    private func findMatchingElement(
        root: AXUIElement,
        criteria: SelectorCriteria,
        depth: Int,
        visited: Int
    ) -> (element: AXUIElement?, visited: Int) {
        if visited > 2048 || depth > 12 {
            return (nil, visited)
        }
        if criteria.matches(element: root) {
            return (root, visited)
        }

        guard let children = copyAttribute(element: root, attribute: kAXChildrenAttribute as CFString) as? [AXUIElement] else {
            return (nil, visited)
        }

        var currentVisited = visited + 1
        for child in children {
            let result = findMatchingElement(root: child, criteria: criteria, depth: depth + 1, visited: currentVisited)
            currentVisited = result.visited
            if let element = result.element {
                return (element, currentVisited)
            }
        }

        return (nil, currentVisited)
    }

    private func findChild(element: AXUIElement, title: String) -> AXUIElement? {
        if let children = copyAttribute(element: element, attribute: kAXChildrenAttribute as CFString) as? [AXUIElement] {
            for child in children {
                if titleValue(for: child) == title {
                    return child
                }
                if let nested = findChild(element: child, title: title) {
                    return nested
                }
            }
        }
        return nil
    }

    private func titleValue(for element: AXUIElement) -> String? {
        if let title = copyAttribute(element: element, attribute: kAXTitleAttribute as CFString) as? String, !title.isEmpty {
            return title
        }
        if let description = copyAttribute(element: element, attribute: kAXDescriptionAttribute as CFString) as? String, !description.isEmpty {
            return description
        }
        if let value = copyAttribute(element: element, attribute: kAXValueAttribute as CFString) as? String, !value.isEmpty {
            return value
        }
        return nil
    }

    private func copyAttribute(element: AXUIElement, attribute: CFString) -> AnyObject? {
        var value: CFTypeRef?
        let result = AXUIElementCopyAttributeValue(element, attribute, &value)
        guard result == .success else {
            return nil
        }
        return value
    }

    private func attributeString(_ attribute: CFString, element: AXUIElement) -> String? {
        guard let value = copyAttribute(element: element, attribute: attribute) else {
            return nil
        }
        if let string = value as? String, !string.isEmpty {
            return string
        }
        if let number = value as? NSNumber {
            return number.stringValue
        }
        return nil
    }

    private func focusedWindowTitle(for application: NSRunningApplication) throws -> String? {
        let appElement = AXUIElementCreateApplication(application.processIdentifier)
        guard let window = copyAttribute(element: appElement, attribute: kAXFocusedWindowAttribute as CFString) as! AXUIElement? else {
            return nil
        }
        return titleValue(for: window)
    }

    private func describeElement(_ element: AXUIElement) -> [String: Any] {
        var description: [String: Any] = [:]
        if let role = attributeString(kAXRoleAttribute as CFString, element: element) {
            description["role"] = role
        }
        if let title = titleValue(for: element) {
            description["title"] = title
        }
        if let identifier = attributeString(kAXIdentifierAttribute as CFString, element: element) {
            description["identifier"] = identifier
        }
        return description
    }

    func takeScreenshot(quality: Double) throws -> [String: Any] {
        #if canImport(ApplicationServices) && canImport(ImageIO) && canImport(UniformTypeIdentifiers)
        let displayID = CGMainDisplayID()
        guard let captured = CGDisplayCreateImage(displayID) else {
            throw RunnerServiceError.executionFailed("failed to capture screenshot")
        }

        // CGDisplayCreateImage returns the frame buffer in *physical pixels* (e.g. 2880x1800
        // on a Retina display), but mouse events in `clickAt` are posted in *logical points*
        // (e.g. 1440x900). If we hand the model a physical-pixel image, every coordinate it
        // returns is off by the display's backing scale factor. Downscale to logical points
        // so the image the model sees shares the coordinate space we click in 1:1.
        let pointWidth: Int
        let pointHeight: Int
        if let mode = CGDisplayCopyDisplayMode(displayID), mode.width > 0, mode.height > 0 {
            pointWidth = mode.width
            pointHeight = mode.height
        } else {
            // Fall back to the captured size (assume non-Retina / 1x) if the mode is unavailable.
            pointWidth = captured.width
            pointHeight = captured.height
        }

        let image: CGImage
        if captured.width != pointWidth || captured.height != pointHeight {
            guard let resized = Self.resize(captured, toWidth: pointWidth, toHeight: pointHeight) else {
                throw RunnerServiceError.executionFailed("failed to downscale screenshot to logical points")
            }
            image = resized
        } else {
            image = captured
        }

        let scale = pointWidth > 0 ? Double(captured.width) / Double(pointWidth) : 1.0

        let data = NSMutableData()
        let jpegType = UTType.jpeg.identifier as CFString
        guard let destination = CGImageDestinationCreateWithData(data, jpegType, 1, nil) else {
            throw RunnerServiceError.executionFailed("failed to create JPEG destination")
        }

        let options: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: quality]
        CGImageDestinationAddImage(destination, image, options as CFDictionary)
        guard CGImageDestinationFinalize(destination) else {
            throw RunnerServiceError.executionFailed("failed to finalize JPEG encoding")
        }

        let base64 = (data as Data).base64EncodedString()

        // `width`/`height` are reported in logical points so the model's coordinates map
        // directly onto `clickAt`. `scale` and the raw pixel dims are included for diagnostics.
        return [
            "action": "screenshot",
            "base64": base64,
            "width": image.width,
            "height": image.height,
            "scale": scale,
            "pixelWidth": captured.width,
            "pixelHeight": captured.height,
            "format": "jpeg",
        ]
        #else
        throw RunnerServiceError.executionFailed("screenshot is only available on macOS")
        #endif
    }

    #if canImport(ApplicationServices)
    /// Downscales a captured frame to the target logical-point dimensions.
    private static func resize(_ image: CGImage, toWidth width: Int, toHeight height: Int) -> CGImage? {
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        guard let context = CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: colorSpace,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else {
            return nil
        }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()
    }
    #endif
}

private struct SelectorCriteria {
    let candidates: [AXSelectorCandidate]

    init(selector: [String: Any]) {
        var parsed: [AXSelectorCandidate] = []
        if let ax = selector["ax"] as? [String: Any],
           let candidate = AXSelectorCandidate(ax: ax)
        {
            parsed.append(candidate)
        }

        for key in ["ranking", "fallbacks"] {
            guard let values = selector[key] as? [[String: Any]] else {
                continue
            }
            for value in values {
                guard (value["kind"] as? String) == "ax",
                      let ax = value["ax"] as? [String: Any],
                      let candidate = AXSelectorCandidate(ax: ax),
                      !parsed.contains(candidate)
                else {
                    continue
                }
                parsed.append(candidate)
            }
        }

        candidates = parsed
    }

    func matches(element: AXUIElement) -> Bool {
        for candidate in candidates where candidate.matches(element: element) {
            return true
        }
        return false
    }
}

private struct AXSelectorCandidate: Equatable {
    let role: String?
    let subrole: String?
    let title: String?
    let description: String?
    let identifier: String?

    init?(ax: [String: Any]) {
        role = ax["role"] as? String
        subrole = ax["subrole"] as? String
        title = ax["title"] as? String
        description = ax["description"] as? String
        identifier = ax["identifier"] as? String

        if role == nil && subrole == nil && title == nil && description == nil && identifier == nil {
            return nil
        }
    }

    func matches(element: AXUIElement) -> Bool {
        if let role, attributeString(kAXRoleAttribute as CFString, element) != role {
            return false
        }
        if let subrole, attributeString(kAXSubroleAttribute as CFString, element) != subrole {
            return false
        }
        if let title, preferredTitle(for: element) != title {
            return false
        }
        if let description, attributeString(kAXDescriptionAttribute as CFString, element) != description {
            return false
        }
        if let identifier, attributeString(kAXIdentifierAttribute as CFString, element) != identifier {
            return false
        }
        return true
    }

    private func preferredTitle(for element: AXUIElement) -> String? {
        attributeString(kAXTitleAttribute as CFString, element)
            ?? attributeString(kAXDescriptionAttribute as CFString, element)
            ?? attributeString(kAXValueAttribute as CFString, element)
    }

    private func attributeString(_ attribute: CFString, _ element: AXUIElement) -> String? {
        var value: CFTypeRef?
        let result = AXUIElementCopyAttributeValue(element, attribute, &value)
        guard result == .success else { return nil }
        if let string = value as? String, !string.isEmpty {
            return string
        }
        if let number = value as? NSNumber {
            return number.stringValue
        }
        return nil
    }
}

private func keyCodeForName(_ name: String) -> CGKeyCode? {
    switch name.lowercased() {
    case "a": return 0
    case "s": return 1
    case "d": return 2
    case "f": return 3
    case "h": return 4
    case "g": return 5
    case "z": return 6
    case "x": return 7
    case "c": return 8
    case "v": return 9
    case "b": return 11
    case "q": return 12
    case "w": return 13
    case "e": return 14
    case "r": return 15
    case "y": return 16
    case "t": return 17
    case "1": return 18
    case "2": return 19
    case "3": return 20
    case "4": return 21
    case "6": return 22
    case "5": return 23
    case "9": return 25
    case "7": return 26
    case "8": return 28
    case "0": return 29
    case "o": return 31
    case "u": return 32
    case "i": return 34
    case "p": return 35
    case "l": return 37
    case "j": return 38
    case "k": return 40
    case "n": return 45
    case "m": return 46
    case "return", "enter", "kp_enter": return 36
    case "tab": return 48
    case "space": return 49
    case "delete", "backspace": return 51
    case "forward_delete", "forwarddelete": return 117
    case "escape", "esc": return 53
    case "left": return 123
    case "right": return 124
    case "down": return 125
    case "up": return 126
    case "home": return 115
    case "end": return 119
    case "page_up", "pageup", "prior": return 116
    case "page_down", "pagedown", "next": return 121
    case "minus": return 27
    case "equal", "equals": return 24
    case "f1": return 122
    case "f2": return 120
    case "f3": return 99
    case "f4": return 118
    case "f5": return 96
    case "f6": return 97
    case "f7": return 98
    case "f8": return 100
    case "f9": return 101
    case "f10": return 109
    case "f11": return 103
    case "f12": return 111
    default: return nil
    }
}
