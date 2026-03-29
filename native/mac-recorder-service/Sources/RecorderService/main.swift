import Foundation

struct BridgeCommand: Decodable {
    let command: String
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

func runBridgeMode() {
    let service = RecorderServiceImpl()
    emitTelemetry(
        phase: "bridge_started",
        extra: [
            "pid": ProcessInfo.processInfo.processIdentifier,
        ]
    )
    RecorderBridgeEmitter.shared.handler = { event in
        writeBridgeMessage(kind: "event", payload: event)
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
    DispatchQueue.global(qos: .userInitiated).async {
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
                    service.bridgeEndCapture { reply in
                        writeBridgeMessage(kind: "capture_stopped", payload: reply)
                        DispatchQueue.main.async {
                            emitTelemetry(phase: "bridge_stopping", extra: ["reason": "stop_command"])
                            CFRunLoopStop(CFRunLoopGetMain())
                        }
                    }
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
} else if CommandLine.arguments.contains("--bridge") {
    runBridgeMode()
} else {
    let service = RecorderService()
    service.run()
}
