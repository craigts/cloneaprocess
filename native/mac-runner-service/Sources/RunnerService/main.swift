import Foundation

let service = RunnerService()
if CommandLine.arguments.contains("--protocol-json") {
    let data = try JSONSerialization.data(withJSONObject: protocolHandshakePayload(), options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data("\n".utf8))
    fflush(stdout)
} else if CommandLine.arguments.contains("--bridge") {
    service.runBridge()
} else {
    service.run()
}
