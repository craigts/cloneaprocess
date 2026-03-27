#if os(macOS)
#if canImport(ApplicationServices)
import ApplicationServices
#endif
import Foundation

@objc protocol EventSinkXPC {
    func onEvent(_ event: [String: Any])
}

@objc protocol RecorderServiceXPC {
    func ping(_ reply: @escaping (String) -> Void)
    func getPermissions(_ reply: @escaping ([String: Bool]) -> Void)
    func beginCapture(_ config: [String: Any], reply: @escaping ([String: Any]) -> Void)
    func endCapture(_ sessionId: String, reply: @escaping ([String: Any]) -> Void)
    func subscribeEvents(_ eventSink: NSXPCListenerEndpoint, reply: @escaping ([String: Any]) -> Void)
    func unsubscribeEvents(_ reply: @escaping ([String: Any]) -> Void)
}

final class RecorderServiceImpl: NSObject, RecorderServiceXPC {
    private var captureSessionId: String?
    private var captureStartedAtMs: UInt64?
    private var emittedEventCount: UInt64 = 0
    private var eventTapPort: CFMachPort?
    private var eventTapRunLoopSource: CFRunLoopSource?
    private var subscriberConnectionByAuditToken: [Data: NSXPCConnection] = [:]

    func ping(_ reply: @escaping (String) -> Void) {
        reply("pong")
    }

    func getPermissions(_ reply: @escaping ([String: Bool]) -> Void) {
        let permissions: [String: Bool] = [
            "accessibility": isAccessibilityGranted(),
            "screenRecording": isScreenRecordingGranted(),
        ]

        reply(permissions)
    }

    func beginCapture(_ config: [String: Any], reply: @escaping ([String: Any]) -> Void) {
        _ = config

        guard captureSessionId == nil else {
            reply([
                "ok": false,
                "error": "capture_already_active",
            ])
            return
        }

        guard isAccessibilityGranted() else {
            reply([
                "ok": false,
                "error": "accessibility_not_granted",
            ])
            return
        }

        let sessionId = "sess_\(UUID().uuidString.lowercased())"
        captureSessionId = sessionId
        captureStartedAtMs = nowMs()
        emittedEventCount = 0

        let tapStarted = startEventTap()
        reply([
            "ok": tapStarted,
            "session_id": sessionId,
            "started_at": captureStartedAtMs ?? nowMs(),
            "error": tapStarted ? NSNull() : "event_tap_start_failed",
        ])
    }

    func endCapture(_ sessionId: String, reply: @escaping ([String: Any]) -> Void) {
        guard captureSessionId == sessionId else {
            reply([
                "ok": false,
                "error": "unknown_session",
            ])
            return
        }

        stopEventTap()
        let endedAt = nowMs()

        reply([
            "ok": true,
            "session_id": sessionId,
            "started_at": captureStartedAtMs ?? endedAt,
            "ended_at": endedAt,
            "event_count": emittedEventCount,
        ])

        captureSessionId = nil
        captureStartedAtMs = nil
        emittedEventCount = 0
    }

    func subscribeEvents(_ eventSink: NSXPCListenerEndpoint, reply: @escaping ([String: Any]) -> Void) {
        guard let callerConnection = NSXPCConnection.current() else {
            reply([
                "ok": false,
                "error": "no_current_connection",
            ])
            return
        }

        let sinkConnection = NSXPCConnection(listenerEndpoint: eventSink)
        sinkConnection.remoteObjectInterface = NSXPCInterface(with: EventSinkXPC.self)
        sinkConnection.invalidationHandler = { [weak self, weak callerConnection] in
            guard let self, let callerConnection else { return }
            self.subscriberConnectionByAuditToken.removeValue(forKey: callerConnection.auditTokenData)
        }
        sinkConnection.resume()

        subscriberConnectionByAuditToken[callerConnection.auditTokenData] = sinkConnection
        reply([
            "ok": true,
        ])
    }

    func unsubscribeEvents(_ reply: @escaping ([String: Any]) -> Void) {
        guard let connection = NSXPCConnection.current() else {
            reply([
                "ok": false,
                "error": "no_current_connection",
            ])
            return
        }

        guard let sinkConnection = subscriberConnectionByAuditToken.removeValue(forKey: connection.auditTokenData) else {
            reply([
                "ok": false,
                "error": "not_subscribed",
            ])
            return
        }

        sinkConnection.invalidate()
        reply([
            "ok": true,
        ])
    }

    private func startEventTap() -> Bool {
        #if canImport(ApplicationServices)
        guard eventTapPort == nil else { return true }

        let mouseAndKeyboardEventsMask =
            (1 << CGEventType.keyDown.rawValue)
            | (1 << CGEventType.keyUp.rawValue)
            | (1 << CGEventType.leftMouseDown.rawValue)
            | (1 << CGEventType.leftMouseUp.rawValue)
            | (1 << CGEventType.rightMouseDown.rawValue)
            | (1 << CGEventType.rightMouseUp.rawValue)
            | (1 << CGEventType.mouseMoved.rawValue)
            | (1 << CGEventType.leftMouseDragged.rawValue)
            | (1 << CGEventType.rightMouseDragged.rawValue)

        let observer = UnsafeMutableRawPointer(Unmanaged.passUnretained(self).toOpaque())

        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .listenOnly,
            eventsOfInterest: CGEventMask(mouseAndKeyboardEventsMask),
            callback: { _, type, event, userInfo in
                guard let userInfo else { return Unmanaged.passUnretained(event) }
                let service = Unmanaged<RecorderServiceImpl>.fromOpaque(userInfo).takeUnretainedValue()
                service.handle(event: event, type: type)
                return Unmanaged.passUnretained(event)
            },
            userInfo: observer
        ) else {
            return false
        }

        guard let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0) else {
            return false
        }

        eventTapPort = tap
        eventTapRunLoopSource = source
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)

        return true
        #else
        return false
        #endif
    }

    private func stopEventTap() {
        #if canImport(ApplicationServices)
        if let tap = eventTapPort {
            CGEvent.tapEnable(tap: tap, enable: false)
        }

        if let source = eventTapRunLoopSource {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
        }

        eventTapRunLoopSource = nil
        eventTapPort = nil
        #endif
    }

    private func handle(event: CGEvent, type: CGEventType) {
        guard captureSessionId != nil else { return }

        let location = event.location
        let eventType = mapEventType(type)
        guard !eventType.isEmpty else { return }

        emittedEventCount += 1
        var payload: [String: Any] = [
            "x": location.x,
            "y": location.y,
        ]

        if type == .keyDown || type == .keyUp {
            payload["key_code"] = event.getIntegerValueField(.keyboardEventKeycode)
        }

        let envelope: [String: Any] = [
            "v": 1,
            "id": "evt_\(UUID().uuidString.lowercased())",
            "ts": nowMs(),
            "type": eventType,
            "payload": payload,
        ]

        emitEvent(envelope)
    }

    private func emitEvent(_ event: [String: Any]) {
        for connection in subscriberConnectionByAuditToken.values {
            guard let sink = connection.remoteObjectProxy as? EventSinkXPC else {
                continue
            }

            sink.onEvent(event)
        }
    }

    private func mapEventType(_ type: CGEventType) -> String {
        switch type {
        case .keyDown: return "key_down"
        case .keyUp: return "key_up"
        case .leftMouseDown: return "mouse_down"
        case .leftMouseUp: return "mouse_up"
        case .rightMouseDown: return "mouse_down"
        case .rightMouseUp: return "mouse_up"
        case .mouseMoved: return "mouse_move"
        case .leftMouseDragged: return "mouse_drag"
        case .rightMouseDragged: return "mouse_drag"
        default: return ""
        }
    }

    private func nowMs() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }

    private func isAccessibilityGranted() -> Bool {
        #if canImport(ApplicationServices)
        return AXIsProcessTrusted()
        #else
        return false
        #endif
    }

    private func isScreenRecordingGranted() -> Bool {
        #if canImport(ApplicationServices)
        return CGPreflightScreenCaptureAccess()
        #else
        return false
        #endif
    }
}

private extension NSXPCConnection {
    var auditTokenData: Data {
        withUnsafeBytes(of: auditToken) { Data($0) }
    }
}

final class RecorderServiceDelegate: NSObject, NSXPCListenerDelegate {
    private let exportedObject = RecorderServiceImpl()

    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection newConnection: NSXPCConnection) -> Bool {
        let interface = NSXPCInterface(with: RecorderServiceXPC.self)
        interface.setClasses(
            [NSDictionary.self, NSString.self, NSNumber.self, NSNull.self],
            for: #selector(RecorderServiceXPC.beginCapture(_:reply:)),
            argumentIndex: 0,
            ofReply: false
        )
        interface.setClasses(
            [NSDictionary.self, NSString.self, NSNumber.self, NSNull.self],
            for: #selector(RecorderServiceXPC.beginCapture(_:reply:)),
            argumentIndex: 0,
            ofReply: true
        )
        interface.setClasses(
            [NSDictionary.self, NSString.self, NSNumber.self, NSNull.self],
            for: #selector(RecorderServiceXPC.endCapture(_:reply:)),
            argumentIndex: 0,
            ofReply: true
        )
        interface.setClasses(
            [NSDictionary.self, NSString.self, NSNumber.self],
            for: #selector(RecorderServiceXPC.subscribeEvents(_:reply:)),
            argumentIndex: 0,
            ofReply: true
        )
        interface.setClasses(
            [NSDictionary.self, NSString.self, NSNumber.self],
            for: #selector(RecorderServiceXPC.unsubscribeEvents(_:)),
            argumentIndex: 0,
            ofReply: true
        )

        newConnection.exportedInterface = interface
        newConnection.exportedObject = exportedObject
        newConnection.resume()
        return true
    }
}

struct RecorderService {
    private let listener: NSXPCListener
    private let delegate = RecorderServiceDelegate()

    init(listener: NSXPCListener = .service()) {
        self.listener = listener
    }

    func run() {
        listener.delegate = delegate
        listener.resume()
        RunLoop.main.run()
    }
}
#else
import Foundation

struct RecorderService {
    func run() {
        print("RecorderService is only available on macOS.")
    }
}
#endif
