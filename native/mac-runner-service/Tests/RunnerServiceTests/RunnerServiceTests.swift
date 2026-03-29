import Foundation
import Testing
@testable import RunnerService

struct MockRunnerActionPerformer: RunnerActionPerforming {
    var failOnKind: String?
    let recorder: InvocationRecorder

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
        recorder.record("selectMenu:\(path.joined(separator: \">\"))")
        if failOnKind == "selectMenu" {
            throw RunnerServiceError.executionFailed("selectMenu failed")
        }
        return ["action": "selectMenu", "path": path]
    }
}

final class InvocationRecorder {
    private(set) var invocations: [String] = []

    func record(_ invocation: String) {
        invocations.append(invocation)
    }
}

@Test func runWorkflowEmitsReplyAndCompletionEvents() throws {
    let recorder = InvocationRecorder()
    let session = RunnerBridgeSession(performer: MockRunnerActionPerformer(failOnKind: nil, recorder: recorder))

    let lines = session.handle(jsonLine: """
    {"id":"req_1","type":"run_workflow","payload":{"workflow_id":"wf_1","run_id":"run_1","steps":[{"kind":"click","selector":{"ax":{"role":"AXButton","title":"Save"}}},{"kind":"setText","selector":{"ax":{"role":"AXTextField","title":"Name"}},"value":{"kind":"literal","value":"Alice"}},{"kind":"selectMenu","path":["File","New"]}]}}
    """)

    #expect(lines.count == 9)
    #expect(recorder.invocations == ["click", "setText:Alice", "selectMenu:File>New"])
    #expect(lines[0].contains("\"ok\":true"))
    #expect(lines[1].contains("\"type\":\"run_started\""))
    #expect(lines[8].contains("\"type\":\"run_completed\""))
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
