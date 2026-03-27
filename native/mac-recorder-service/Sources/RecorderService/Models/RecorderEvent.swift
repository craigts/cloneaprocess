import Foundation

struct RecorderEvent: Codable {
    let id: String
    let type: String
    let timestamp: UInt64
}

