import Foundation
import Testing
@testable import RecorderService

@Test func protocolHandshakeIncludesExpectedVersionAndCapabilities() throws {
    let payload = protocolHandshakePayload()

    #expect(payload["protocol_version"] as? Int == 1)
    #expect(payload["protocol_min"] as? Int == 1)

    let capabilities = payload["capabilities"] as? [String]
    #expect(capabilities?.contains("event_stream") == true)
    #expect(capabilities?.contains("permissions") == true)
    #expect(capabilities?.contains("ax_snapshot") == true)
    #expect(capabilities?.contains("screen_frame") == true)
    #expect(capabilities?.contains("subprocess_bridge") == true)
}

@Test func protocolHandshakePayloadSerializesToValidJson() throws {
    let payload = protocolHandshakePayload()

    #expect(JSONSerialization.isValidJSONObject(payload))
    let data = try JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys])
    let object = try JSONSerialization.jsonObject(with: data) as? [String: Any]

    #expect(object?["protocol_version"] as? Int == 1)
}
