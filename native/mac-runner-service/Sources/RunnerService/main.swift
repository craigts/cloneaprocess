import Foundation

let service = RunnerService()
if CommandLine.arguments.contains("--bridge") {
    service.runBridge()
} else {
    service.run()
}
