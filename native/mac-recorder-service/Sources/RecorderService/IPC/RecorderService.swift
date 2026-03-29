#if os(macOS)
#if canImport(ApplicationServices)
import ApplicationServices
#endif
import Foundation
#if canImport(AppKit)
import AppKit
#endif
#if canImport(CoreGraphics)
import CoreGraphics
#endif
#if canImport(ImageIO)
import ImageIO
#endif
#if canImport(UniformTypeIdentifiers)
import UniformTypeIdentifiers
#endif
#if canImport(ScreenCaptureKit)
import ScreenCaptureKit
#endif

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
    private let enableEventTap = true
    private let enableAXSnapshots = false
    private let enableKeyframes = false
    private var captureSessionId: String?
    private var captureStartedAtMs: UInt64?
    private var emittedEventCount: UInt64 = 0
    private var emittedFrameCount: UInt64 = 0
    private var eventTapPort: CFMachPort?
    private var eventTapRunLoopSource: CFRunLoopSource?
    private var subscriberConnectionByIdentifier: [ObjectIdentifier: NSXPCConnection] = [:]
    private var frameOutputDirectoryURL: URL?
    private let eventDeliveryQueue = DispatchQueue(label: "com.cloneaprocess.recorder.event-delivery")
    private let frameCaptureQueue = DispatchQueue(label: "com.cloneaprocess.recorder.frame-capture")
    private let accessibilityQueue = DispatchQueue(label: "com.cloneaprocess.recorder.accessibility")

    func ping(_ reply: @escaping (String) -> Void) {
        reply("pong")
    }

    func bridgeAccessibilityGranted() -> Bool {
        isAccessibilityGranted()
    }

    func bridgeScreenRecordingGranted() -> Bool {
        isScreenRecordingGranted()
    }

    func bridgeBeginCapture(config: [String: Any], reply: @escaping ([String: Any]) -> Void) {
        beginCapture(config, reply: reply)
    }

    func bridgeEndCapture(reply: @escaping ([String: Any]) -> Void) {
        guard let sessionId = captureSessionId else {
            reply([
                "ok": false,
                "error": "no_active_session",
            ])
            return
        }

        endCapture(sessionId, reply: reply)
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

        guard isScreenRecordingGranted() else {
            reply([
                "ok": false,
                "error": "screen_recording_not_granted",
            ])
            return
        }

        let sessionId = "sess_\(UUID().uuidString.lowercased())"
        let frameDirectoryURL = frameDirectory(for: sessionId)

        do {
            try FileManager.default.createDirectory(at: frameDirectoryURL, withIntermediateDirectories: true)
        } catch {
            reply([
                "ok": false,
                "error": "frame_directory_create_failed",
            ])
            return
        }

        captureSessionId = sessionId
        captureStartedAtMs = nowMs()
        emittedEventCount = 0
        emittedFrameCount = 0
        frameOutputDirectoryURL = frameDirectoryURL

        let tapStarted = enableEventTap ? startEventTap() : true
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
            "frame_count": emittedFrameCount,
        ])

        captureSessionId = nil
        captureStartedAtMs = nil
        emittedEventCount = 0
        emittedFrameCount = 0
        frameOutputDirectoryURL = nil
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
            self.subscriberConnectionByIdentifier.removeValue(forKey: ObjectIdentifier(callerConnection))
        }
        sinkConnection.resume()

        subscriberConnectionByIdentifier[ObjectIdentifier(callerConnection)] = sinkConnection
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

        guard let sinkConnection = subscriberConnectionByIdentifier.removeValue(forKey: ObjectIdentifier(connection)) else {
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

        eventDeliveryQueue.async { [weak self] in
            self?.emitEvent(envelope)
        }

        if shouldCaptureAXSnapshot(for: type) {
            accessibilityQueue.async { [weak self] in
                self?.emitAXSnapshot(at: location)
            }
        }

        if shouldCaptureKeyframe(for: type) {
            captureAndEmitKeyframe()
        }
    }

    private func shouldCaptureAXSnapshot(for eventType: CGEventType) -> Bool {
        guard enableAXSnapshots else { return false }
        switch eventType {
        case .leftMouseDown, .rightMouseDown:
            return true
        default:
            return false
        }
    }

    private func shouldCaptureKeyframe(for eventType: CGEventType) -> Bool {
        guard enableKeyframes else { return false }
        switch eventType {
        case .leftMouseDown, .rightMouseDown, .keyDown:
            return true
        default:
            return false
        }
    }

    private func captureAndEmitKeyframe() {
        guard let sessionId = captureSessionId else { return }
        guard let outputDirectoryURL = frameOutputDirectoryURL else { return }

        frameCaptureQueue.async { [weak self] in
            guard let self else { return }
            guard self.isCaptureSessionActive(sessionId: sessionId, outputDirectoryURL: outputDirectoryURL) else { return }

            let frameId = "frm_\(UUID().uuidString.lowercased())"
            let frameURL = outputDirectoryURL.appendingPathComponent("\(frameId).jpg")

            guard let screenshotData = self.captureScreenshotJPEGData() else { return }

            do {
                try screenshotData.write(to: frameURL, options: .atomic)
            } catch {
                return
            }

            guard self.isCaptureSessionActive(sessionId: sessionId, outputDirectoryURL: outputDirectoryURL) else { return }

            self.emittedFrameCount += 1
            let frameEvent: [String: Any] = [
                "v": 1,
                "id": "evt_\(UUID().uuidString.lowercased())",
                "ts": self.nowMs(),
                "type": "screen_frame",
                "payload": [
                    "session_id": sessionId,
                    "frame_id": frameId,
                    "path": frameURL.path,
                ],
            ]

            self.emitEvent(frameEvent)
        }
    }

    private func emitAXSnapshot(at location: CGPoint) {
        #if canImport(ApplicationServices)
        let systemWideElement = AXUIElementCreateSystemWide()
        var resolvedElement: AXUIElement?
        let result = AXUIElementCopyElementAtPosition(
            systemWideElement,
            Float(location.x),
            Float(location.y),
            &resolvedElement
        )

        guard result == .success else { return }
        guard let element = resolvedElement else { return }
        guard let snapshotPayload = axSnapshotPayload(for: element, location: location) else { return }

        let snapshotEvent: [String: Any] = [
            "v": 1,
            "id": "evt_\(UUID().uuidString.lowercased())",
            "ts": nowMs(),
            "type": "ax_snapshot",
            "payload": snapshotPayload,
        ]

        emitEvent(snapshotEvent)
        #endif
    }

    private func axSnapshotPayload(for element: AXUIElement, location: CGPoint) -> [String: Any]? {
        #if canImport(ApplicationServices)
        let role = axStringAttribute(kAXRoleAttribute as CFString, from: element)
        let subrole = axStringAttribute(kAXSubroleAttribute as CFString, from: element)
        let title = preferredAXTitle(for: element)
        let description = axStringAttribute(kAXDescriptionAttribute as CFString, from: element)

        if role == nil && title == nil && description == nil {
            return nil
        }

        var pid: pid_t = 0
        let pidResult = AXUIElementGetPid(element, &pid)
        let bundleIdentifier: String?
        if pidResult == .success, pid > 0 {
            #if canImport(AppKit)
            bundleIdentifier = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier
            #else
            bundleIdentifier = nil
            #endif
        } else {
            bundleIdentifier = nil
        }

        var selectorAX: [String: Any] = [:]
        if let role {
            selectorAX["role"] = role
        }
        if let subrole {
            selectorAX["subrole"] = subrole
        }
        if let title {
            selectorAX["title"] = title
        }
        if let description {
            selectorAX["description"] = description
        }

        var selector: [String: Any] = [:]
        if let bundleIdentifier {
            selector["target_app"] = ["bundle_id": bundleIdentifier]
        }
        if !selectorAX.isEmpty {
            selector["ax"] = selectorAX
        }

        var payload: [String: Any] = [
            "snapshot_id": "ax_\(UUID().uuidString.lowercased())",
            "x": location.x,
            "y": location.y,
        ]

        if let bundleIdentifier {
            payload["bundle_id"] = bundleIdentifier
        }
        if let role {
            payload["role"] = role
        }
        if let subrole {
            payload["subrole"] = subrole
        }
        if let title {
            payload["title"] = title
        }
        if let description {
            payload["description"] = description
        }
        if !selector.isEmpty {
            payload["selector"] = selector
        }

        return payload
        #else
        return nil
        #endif
    }

    private func preferredAXTitle(for element: AXUIElement) -> String? {
        #if canImport(ApplicationServices)
        if let title = axStringAttribute(kAXTitleAttribute as CFString, from: element), !title.isEmpty {
            return title
        }
        if let description = axStringAttribute(kAXDescriptionAttribute as CFString, from: element), !description.isEmpty {
            return description
        }
        if let value = axStringAttribute(kAXValueAttribute as CFString, from: element), !value.isEmpty {
            return value
        }
        return nil
        #else
        return nil
        #endif
    }

    private func axStringAttribute(_ attribute: CFString, from element: AXUIElement) -> String? {
        #if canImport(ApplicationServices)
        var rawValue: CFTypeRef?
        let result = AXUIElementCopyAttributeValue(element, attribute, &rawValue)
        guard result == .success else { return nil }
        guard let rawValue else { return nil }

        if CFGetTypeID(rawValue) == CFStringGetTypeID() {
            return rawValue as? String
        }

        if let attributedValue = rawValue as? NSAttributedString {
            return attributedValue.string
        }

        if let number = rawValue as? NSNumber {
            return number.stringValue
        }

        return nil
        #else
        return nil
        #endif
    }

    private func isCaptureSessionActive(sessionId: String, outputDirectoryURL: URL) -> Bool {
        captureSessionId == sessionId && frameOutputDirectoryURL == outputDirectoryURL
    }

    private func captureScreenshotJPEGData() -> Data? {
        #if canImport(ApplicationServices)
        guard let image = CGDisplayCreateImage(CGMainDisplayID()) else {
            return nil
        }

        return Self.jpegData(from: image)
        #else
        return nil
        #endif
    }

    private static func jpegData(from image: CGImage) -> Data? {
        #if canImport(ImageIO)
        let data = NSMutableData()
        #if canImport(UniformTypeIdentifiers)
        let jpegType = UTType.jpeg.identifier as CFString
        #else
        let jpegType = "public.jpeg" as CFString
        #endif

        guard let destination = CGImageDestinationCreateWithData(data, jpegType, 1, nil) else {
            return nil
        }

        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            return nil
        }

        return data as Data
        #else
        return nil
        #endif
    }

    private func frameDirectory(for sessionId: String) -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("cloneaprocess-recordings", isDirectory: true)
            .appendingPathComponent(sessionId, isDirectory: true)
            .appendingPathComponent("frames", isDirectory: true)
    }

    private func emitEvent(_ event: [String: Any]) {
        for connection in subscriberConnectionByIdentifier.values {
            guard let sink = connection.remoteObjectProxy as? EventSinkXPC else {
                continue
            }

            sink.onEvent(event)
        }

        RecorderBridgeEmitter.shared.emit(event)
    }

    private func mapEventType(_ type: CGEventType) -> String {
        switch type {
        case .keyDown: return "key_down"
        case .keyUp: return "key_up"
        case .leftMouseDown: return "mouse_down"
        case .leftMouseUp: return "mouse_up"
        case .rightMouseDown: return "mouse_down"
        case .rightMouseUp: return "mouse_up"
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

final class RecorderServiceDelegate: NSObject, NSXPCListenerDelegate {
    private let exportedObject = RecorderServiceImpl()

    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection newConnection: NSXPCConnection) -> Bool {
        let interface = NSXPCInterface(with: RecorderServiceXPC.self)
        let objectClasses = xpcAllowedClasses([NSDictionary.self, NSString.self, NSNumber.self, NSNull.self])
        let replyClasses = xpcAllowedClasses([NSDictionary.self, NSString.self, NSNumber.self])
        interface.setClasses(
            objectClasses,
            for: #selector(RecorderServiceXPC.beginCapture(_:reply:)),
            argumentIndex: 0,
            ofReply: false
        )
        interface.setClasses(
            objectClasses,
            for: #selector(RecorderServiceXPC.beginCapture(_:reply:)),
            argumentIndex: 0,
            ofReply: true
        )
        interface.setClasses(
            objectClasses,
            for: #selector(RecorderServiceXPC.endCapture(_:reply:)),
            argumentIndex: 0,
            ofReply: true
        )
        interface.setClasses(
            replyClasses,
            for: #selector(RecorderServiceXPC.subscribeEvents(_:reply:)),
            argumentIndex: 0,
            ofReply: true
        )
        interface.setClasses(
            replyClasses,
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

private func xpcAllowedClasses(_ classes: [AnyClass]) -> Set<AnyHashable> {
    Set(classes.map { AnyHashable(ObjectIdentifier($0)) })
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

enum RecorderBridgeEmitter {
    static let shared = RecorderBridgeEmitterState()
}

final class RecorderBridgeEmitterState: @unchecked Sendable {
    private let queue = DispatchQueue(label: "com.cloneaprocess.recorder.bridge-emitter")
    var handler: (([String: Any]) -> Void)?

    func emit(_ event: [String: Any]) {
        queue.async { [handler] in
            handler?(event)
        }
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
