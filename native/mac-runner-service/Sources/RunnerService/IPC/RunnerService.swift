import Foundation
#if canImport(ApplicationServices)
import ApplicationServices
#endif
#if canImport(AppKit)
import AppKit
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
    "subprocess_bridge",
]

protocol RunnerActionPerforming {
    func focusWindow(bundleId: String, title: String?) throws -> [String: Any]
    func click(selector: [String: Any]) throws -> [String: Any]
    func setText(selector: [String: Any], value: String) throws -> [String: Any]
    func selectMenu(path: [String]) throws -> [String: Any]
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
            guard let selector = step["selector"] as? [String: Any] else {
                throw RunnerServiceError.invalidRequest("setText step requires selector")
            }
            guard
                let value = step["value"] as? [String: Any],
                let kind = value["kind"] as? String,
                kind == "literal",
                let literalValue = value["value"] as? String
            else {
                throw RunnerServiceError.invalidRequest("setText step requires a literal value")
            }
            return try performer.setText(selector: selector, value: literalValue)
        case "selectMenu":
            guard let path = step["path"] as? [String], !path.isEmpty else {
                throw RunnerServiceError.invalidRequest("selectMenu step requires a non-empty path")
            }
            return try performer.selectMenu(path: path)
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
        let input = FileHandle.standardInput
        while true {
            let chunkData = input.availableData
            guard !chunkData.isEmpty else { break }
            guard let chunk = String(data: chunkData, encoding: .utf8) else { continue }
            for rawLine in chunk.split(separator: "\n") {
                let lines = session.handle(jsonLine: String(rawLine))
                for line in lines {
                    FileHandle.standardOutput.write(Data(line.utf8))
                    FileHandle.standardOutput.write(Data("\n".utf8))
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

    func setText(selector: [String: Any], value: String) throws -> [String: Any] {
        let element = try resolveElement(selector: selector)
        let result = AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFTypeRef)
        guard result == .success else {
            throw RunnerServiceError.executionFailed("set value failed with code \(result.rawValue)")
        }
        return ["action": "setText", "value": value]
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
