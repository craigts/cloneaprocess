import Foundation

struct BridgeCommand: Decodable {
    let command: String
}

private func argumentValue(for name: String) -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: name) else {
        return nil
    }
    let valueIndex = CommandLine.arguments.index(after: index)
    guard valueIndex < CommandLine.arguments.endIndex else {
        return nil
    }
    return CommandLine.arguments[valueIndex]
}

private func writeStandardError(_ message: String) {
    FileHandle.standardError.write(Data(message.utf8))
    FileHandle.standardError.write(Data("\n".utf8))
}

private func writeBridgeMessage(kind: String, payload: [String: Any]) {
    let message: [String: Any] = [
        "kind": kind,
        "payload": payload,
    ]

    guard JSONSerialization.isValidJSONObject(message),
          let data = try? JSONSerialization.data(withJSONObject: message, options: []),
          let line = String(data: data, encoding: .utf8)
    else {
        return
    }

    FileHandle.standardOutput.write(Data(line.utf8))
    FileHandle.standardOutput.write(Data("\n".utf8))
}

private func emitTelemetry(phase: String, extra: [String: Any] = [:]) {
    var payload = extra
    payload["phase"] = phase
    payload["ts"] = UInt64(Date().timeIntervalSince1970 * 1000)
    writeBridgeMessage(kind: "telemetry", payload: payload)
}

final class BridgeStopCoordinator: @unchecked Sendable {
    private let service: RecorderServiceImpl

    init(service: RecorderServiceImpl) {
        self.service = service
    }

    func stopCaptureAndExitBridge(reason: String) {
        DispatchQueue.main.async {
            self.service.bridgeEndCapture { reply in
                writeBridgeMessage(kind: "capture_stopped", payload: reply)
                emitTelemetry(phase: "bridge_stopping", extra: ["reason": reason])
                CFRunLoopStop(CFRunLoopGetMain())
            }
        }
    }
}

func runBridgeMode() {
    let service = RecorderServiceImpl()
    let stopCoordinator = BridgeStopCoordinator(service: service)
    emitTelemetry(
        phase: "bridge_started",
        extra: [
            "pid": ProcessInfo.processInfo.processIdentifier,
        ]
    )
    RecorderBridgeEmitter.shared.handler = { event in
        writeBridgeMessage(kind: "event", payload: event.value)
    }
    let permissions = [
        "accessibility": service.bridgeAccessibilityGranted(),
        "screenRecording": service.bridgeScreenRecordingGranted(),
    ]
    writeBridgeMessage(kind: "permissions", payload: permissions)

    service.bridgeBeginCapture(config: [:]) { reply in
        writeBridgeMessage(kind: "capture_started", payload: reply)
        emitTelemetry(
            phase: "capture_started_reply",
            extra: [
                "ok": reply["ok"] as? Bool ?? false,
                "error": reply["error"] ?? NSNull(),
            ]
        )
        if let ok = reply["ok"] as? Bool, !ok {
            DispatchQueue.main.async {
                emitTelemetry(phase: "bridge_stopping", extra: ["reason": "capture_start_failed"])
                CFRunLoopStop(CFRunLoopGetMain())
            }
        }
    }

    let input = FileHandle.standardInput
    let inputThread = Thread {
        while let lineData = try? input.read(upToCount: 4096), !lineData.isEmpty {
            guard let line = String(data: lineData, encoding: .utf8) else { continue }
            for rawLine in line.split(separator: "\n") {
                guard let commandData = rawLine.data(using: .utf8),
                      let command = try? JSONDecoder().decode(BridgeCommand.self, from: commandData)
                else {
                    emitTelemetry(phase: "invalid_command")
                    writeStandardError("bridge invalid command: \(rawLine)")
                    continue
                }

                if command.command == "stop" {
                    emitTelemetry(phase: "stop_command_received")
                    stopCoordinator.stopCaptureAndExitBridge(reason: "stop_command")
                    return
                }

                emitTelemetry(phase: "unknown_command", extra: ["command": command.command])
                writeStandardError("bridge unknown command: \(command.command)")
            }
        }

        DispatchQueue.main.async {
            emitTelemetry(phase: "bridge_stopping", extra: ["reason": "stdin_closed"])
            CFRunLoopStop(CFRunLoopGetMain())
        }
    }
    inputThread.qualityOfService = .userInitiated
    inputThread.start()

    RunLoop.main.run()
}

if CommandLine.arguments.contains("--permissions-json") {
    let service = RecorderServiceImpl()
    let permissions = [
        "accessibility": service.bridgeAccessibilityGranted(),
        "screenRecording": service.bridgeScreenRecordingGranted(),
    ]
    let data = try JSONSerialization.data(withJSONObject: permissions, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
} else if CommandLine.arguments.contains("--protocol-json") {
    let data = try JSONSerialization.data(withJSONObject: protocolHandshakePayload(), options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
} else if CommandLine.arguments.contains("--bridge") {
    runBridgeMode()
} else if let machServiceName = argumentValue(for: "--mach-service-name") {
    let service = RecorderService(machServiceName: machServiceName)
    service.run()
} else {
    let service = RecorderService()
    service.run()
}
