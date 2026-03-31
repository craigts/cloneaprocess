import Foundation
import Testing
@testable import RunnerService

struct MockRunnerActionPerformer: RunnerActionPerforming {
    var failOnKind: String?
    let recorder: InvocationRecorder

    func focusWindow(bundleId: String, title: String?) throws -> [String: Any] {
        recorder.record("focusWindow:\(bundleId)")
        if failOnKind == "focusWindow" {
            throw RunnerServiceError.executionFailed("focusWindow failed")
        }
        var payload: [String: Any] = ["action": "focusWindow", "bundleId": bundleId]
        if let title {
            payload["title"] = title
        }
        return payload
    }

    func click(selector: [String: Any]) throws -> [String: Any] {
        recorder.record("click")
        if failOnKind == "click" {
            throw RunnerServiceError.executionFailed("click failed")
        }
        return ["action": "click", "selector": selector]
    }

    func setText(selector: [String: Any], value: String) throws -> [String: Any] {
        recorder.record("setText:\(value)")
        if failOnKind == "setText" {
            throw RunnerServiceError.executionFailed("setText failed")
        }
        return ["action": "setText", "value": value]
    }

    func selectMenu(path: [String]) throws -> [String: Any] {
        recorder.record("selectMenu:\(path.joined(separator: ">"))")
        if failOnKind == "selectMenu" {
            throw RunnerServiceError.executionFailed("selectMenu failed")
        }
        return ["action": "selectMenu", "path": path]
    }

    func waitForCondition(condition: [String: Any], timeoutMs: UInt64) throws -> [String: Any] {
        let kind = condition["kind"] as? String ?? "unknown"
        recorder.record("waitFor:\(kind)")
        if failOnKind == "waitFor" {
            throw RunnerServiceError.executionFailed("waitFor failed")
        }
        return ["action": "waitFor", "conditionKind": kind, "timeoutMs": timeoutMs]
    }

    func assertCondition(condition: [String: Any]) throws -> [String: Any] {
        let kind = condition["kind"] as? String ?? "unknown"
        recorder.record("assert:\(kind)")
        if failOnKind == "assert" {
            throw RunnerServiceError.executionFailed("assert failed")
        }
        return ["action": "assert", "conditionKind": kind]
    }
}

final class InvocationRecorder {
    private(set) var invocations: [String] = []

    func record(_ invocation: String) {
        invocations.append(invocation)
    }
}

@Test func pingReplyIncludesProtocolMetadata() throws {
    let recorder = InvocationRecorder()
    let session = RunnerBridgeSession(performer: MockRunnerActionPerformer(failOnKind: nil, recorder: recorder))

    let lines = session.handle(jsonLine: """
    {"id":"req_ping","type":"ping","payload":{}}
    """)

    #expect(lines.count == 1)
    #expect(lines[0].contains("\"ok\":true"))
    #expect(lines[0].contains("\"protocol_version\":1"))
    #expect(lines[0].contains("\"protocol_min\":1"))
    #expect(lines[0].contains("\"capabilities\""))
    #expect(lines[0].contains("run_workflow"))
}

@Test func runWorkflowEmitsReplyAndCompletionEvents() throws {
    let recorder = InvocationRecorder()
    let session = RunnerBridgeSession(performer: MockRunnerActionPerformer(failOnKind: nil, recorder: recorder))

    let lines = session.handle(jsonLine: """
    {"id":"req_1","type":"run_workflow","payload":{"workflow_id":"wf_1","run_id":"run_1","steps":[{"kind":"focusWindow","bundleId":"com.apple.TextEdit"},{"kind":"click","selector":{"ax":{"role":"AXButton","title":"Save"}}},{"kind":"setText","selector":{"ax":{"role":"AXTextField","title":"Name"}},"value":{"kind":"literal","value":"Alice"}},{"kind":"selectMenu","path":["File","New"]}]}}
    """)

    #expect(lines.count == 11)
    #expect(recorder.invocations == ["focusWindow:com.apple.TextEdit", "click", "setText:Alice", "selectMenu:File>New"])
    #expect(lines[0].contains("\"ok\":true"))
    #expect(lines[1].contains("\"type\":\"run_started\""))
    #expect(lines[10].contains("\"type\":\"run_completed\""))
}

@Test func runWorkflowSupportsVerificationSteps() throws {
    let recorder = InvocationRecorder()
    let session = RunnerBridgeSession(performer: MockRunnerActionPerformer(failOnKind: nil, recorder: recorder))

    let lines = session.handle(jsonLine: """
    {"id":"req_verify","type":"run_workflow","payload":{"workflow_id":"wf_verify","run_id":"run_verify","steps":[{"kind":"waitFor","condition":{"kind":"elementPresent","selector":{"ax":{"role":"AXButton","title":"Save"}}},"timeoutMs":250},{"kind":"assert","condition":{"kind":"textEquals","selector":{"ax":{"role":"AXTextField","title":"Name"}},"value":"Alice"}}]}}
    """)

    #expect(recorder.invocations == ["waitFor:elementPresent", "assert:textEquals"])
    #expect(lines.contains(where: { $0.contains("\"type\":\"run_completed\"") }))
}

@Test func runWorkflowFailureEmitsRunFailed() throws {
    let recorder = InvocationRecorder()
    let session = RunnerBridgeSession(performer: MockRunnerActionPerformer(failOnKind: "setText", recorder: recorder))

    let lines = session.handle(jsonLine: """
    {"id":"req_2","type":"run_workflow","payload":{"workflow_id":"wf_2","run_id":"run_2","steps":[{"kind":"click","selector":{"ax":{"role":"AXButton","title":"Save"}}},{"kind":"setText","selector":{"ax":{"role":"AXTextField","title":"Name"}},"value":{"kind":"literal","value":"Alice"}}]}}
    """)

    #expect(recorder.invocations == ["click", "setText:Alice"])
    #expect(lines.contains(where: { $0.contains("\"type\":\"run_failed\"") }))
    #expect(lines.last?.contains("\"type\":\"run_failed\"") == true)
}

@Test func invalidRequestGetsErrorReply() throws {
    let recorder = InvocationRecorder()
    let session = RunnerBridgeSession(performer: MockRunnerActionPerformer(failOnKind: nil, recorder: recorder))

    let lines = session.handle(jsonLine: "{\"id\":\"req_invalid\",\"type\":\"run_workflow\",\"payload\":{}}")

    #expect(lines.count == 1)
    #expect(lines[0].contains("\"ok\":false"))
    #expect(lines[0].contains("steps array"))
}
