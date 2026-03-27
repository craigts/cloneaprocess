import Foundation

struct RunnerEvent: Codable {
    let id: String
    let type: String
    let timestamp: UInt64
}
