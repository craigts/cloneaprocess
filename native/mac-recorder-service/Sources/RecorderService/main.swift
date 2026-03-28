import Foundation

struct BridgeCommand: Decodable {
    let command: String
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

func runBridgeMode() {
    let service = RecorderServiceImpl()
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
    }

    let input = FileHandle.standardInput
    DispatchQueue.global(qos: .userInitiated).async {
        while let lineData = try? input.read(upToCount: 4096), !lineData.isEmpty {
            guard let line = String(data: lineData, encoding: .utf8) else { continue }
            for rawLine in line.split(separator: "\n") {
                guard let commandData = rawLine.data(using: .utf8),
                      let command = try? JSONDecoder().decode(BridgeCommand.self, from: commandData)
                else {
                    continue
                }

                if command.command == "stop" {
                    service.bridgeEndCapture { reply in
                        writeBridgeMessage(kind: "capture_stopped", payload: reply)
                        DispatchQueue.main.async {
                            CFRunLoopStop(CFRunLoopGetMain())
                        }
                    }
                    return
                }
            }
        }

        DispatchQueue.main.async {
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
