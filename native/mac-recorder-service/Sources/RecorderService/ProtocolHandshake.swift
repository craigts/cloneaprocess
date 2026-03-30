import Foundation

let recorderProtocolVersion = 1
let recorderProtocolMinimumVersion = 1
let recorderProtocolCapabilities = [
    "event_stream",
    "permissions",
    "ax_snapshot",
    "screen_frame",
    "subprocess_bridge",
]

func protocolHandshakePayload() -> [String: Any] {
    [
        "protocol_version": recorderProtocolVersion,
        "protocol_min": recorderProtocolMinimumVersion,
        "capabilities": recorderProtocolCapabilities,
    ]
}
